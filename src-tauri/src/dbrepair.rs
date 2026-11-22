//! Detecting and repairing a damaged `library.sqlite`.
//!
//! - [`quick_check`] runs `PRAGMA quick_check` and returns its findings (empty when healthy).
//! - [`repair_library`] builds a fresh database next to a damaged one: the current schema,
//!   every readable row of every table (checked against the column types and constraints, and
//!   salvaged row by row where a table's b-tree is damaged), screenshot rows rebuilt from the
//!   files in the screenshots folder and, optionally, merged from a known-good backup. The
//!   source is only ever opened read-only; the result is checked with `PRAGMA integrity_check`.
//! - [`open_or_repair`] is the start-up path: a library that fails the quick check is copied
//!   with the SQLite online backup API, repaired into a new file and swapped in before any
//!   connection to it is kept, and the damaged files are kept next to it.
//! - [`backup_to`] makes every app-made copy of the library with the online backup API, so a
//!   copy always holds a consistent snapshot including what is still in the WAL.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, DatabaseName, OpenFlags};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Most findings read from `quick_check` / `integrity_check`.
const CHECK_LIMIT: u32 = 200;
/// Rowids probed past the highest one seen when salvaging a damaged table.
const PROBE_MARGIN: i64 = 1_000;
/// Upper bound on rowid probes per table.
const MAX_PROBES: i64 = 2_000_000;

/// `PRAGMA quick_check` findings; empty when the database is healthy. A database that can't
/// even be read (not a database, corrupt header) is reported as one finding.
pub fn quick_check(conn: &Connection) -> Vec<String> {
    check(conn, "quick_check")
}

/// `PRAGMA integrity_check` findings; empty when the database is healthy.
pub fn integrity_check(conn: &Connection) -> Vec<String> {
    check(conn, "integrity_check")
}

fn check(conn: &Connection, pragma: &str) -> Vec<String> {
    let run = || -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare(&format!("PRAGMA {pragma}({CHECK_LIMIT})"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    match run() {
        Ok(rows) if rows.len() == 1 && rows[0] == "ok" => Vec::new(),
        Ok(rows) if rows.is_empty() => vec![format!("{pragma} returned nothing")],
        Ok(rows) => rows,
        Err(e) => vec![format!("{pragma} failed: {e}")],
    }
}

/// Copies the main database of `conn` to `dest` with the SQLite online backup API. Unlike a
/// file copy, the copy is a consistent snapshot that includes committed pages still in the
/// WAL, and it never races with a checkpoint.
pub fn backup_to(conn: &Connection, dest: &Path) -> Result<()> {
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    conn.backup(DatabaseName::Main, dest, None)
        .with_context(|| format!("backing up the library to {}", dest.display()))
}

/// An SQLite URI for a file path (`file:///C:/x/library.sqlite` on Windows).
fn file_uri(path: &Path, query: &str) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:");
    if !raw.starts_with('/') {
        out.push_str("///");
    } else {
        out.push_str("//");
    }
    for c in raw.chars() {
        match c {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            _ => out.push(c),
        }
    }
    if !query.is_empty() {
        out.push('?');
        out.push_str(query);
    }
    out
}

/// Opens `path` read-only. With `ignore_wal` the main file is read on its own (`immutable=1`):
/// a `-wal` next to it is neither replayed nor touched. Use it when the WAL is known to be
/// older than the main file (a stale WAL left behind by copying or moving files by hand).
pub fn open_source(path: &Path, ignore_wal: bool) -> Result<Connection> {
    if !path.is_file() {
        bail!("{} does not exist", path.display());
    }
    let query = if ignore_wal { "immutable=1" } else { "mode=ro" };
    let conn = Connection::open_with_flags(
        file_uri(path, query),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {} read-only", path.display()))?;
    Ok(conn)
}

#[derive(Debug, Clone, Default)]
pub struct RepairOptions {
    /// Read the main file alone and ignore its `-wal` (see [`open_source`]).
    pub ignore_wal: bool,
    /// Leave `cover_cache`, `game_metadata` and `wikimedia_match` empty; they are refilled from
    /// the network.
    pub drop_caches: bool,
    /// The screenshots folder: `game_screenshots` rows are rebuilt for files no row refers to.
    pub screenshots_dir: Option<PathBuf>,
    /// A known-good copy of the library: screenshot rows it has (whose files still exist) are
    /// merged in when the damaged library lost them.
    pub known_good: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableReport {
    pub name: String,
    /// The damaged library has this table (older libraries lack tables added later).
    pub in_source: bool,
    /// Damaged in the source (named by the integrity check, unreadable, or holding rows that
    /// fail validation), so salvaged row by row.
    pub damaged: bool,
    /// Rows written to the repaired database.
    pub copied: usize,
    /// Rows that could be read but failed validation or a uniqueness constraint.
    pub rejected: usize,
    /// Rowids that could not be read at all.
    pub unreadable: usize,
    /// Left empty on purpose (a cache, with `drop_caches`).
    pub dropped: bool,
    /// Rows added from the known-good backup.
    pub from_backup: usize,
    /// Rows rebuilt from files on disk.
    pub from_files: usize,
    /// Rows deleted afterwards because what they referred to was lost.
    pub orphans_removed: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RepairReport {
    pub source_findings: Vec<String>,
    pub tables: Vec<TableReport>,
    /// Empty when the repaired database passed `integrity_check`.
    pub integrity: Vec<String>,
}

impl RepairReport {
    pub fn healthy(&self) -> bool {
        self.integrity.is_empty()
    }

    #[cfg(test)]
    pub fn table(&self, name: &str) -> Option<&TableReport> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// One line per table that lost or gained rows, for logs and the user notice.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        for t in &self.tables {
            if t.dropped {
                lines.push(format!(
                    "{}: cleared (cache, rebuilt automatically)",
                    t.name
                ));
            } else if t.damaged || t.rejected > 0 || t.unreadable > 0 || t.orphans_removed > 0 {
                let mut parts = vec![format!("{} kept", t.copied)];
                if t.unreadable > 0 {
                    parts.push(format!("{} unreadable", t.unreadable));
                }
                if t.rejected > 0 {
                    parts.push(format!("{} invalid", t.rejected));
                }
                if t.from_backup > 0 {
                    parts.push(format!("{} from backup", t.from_backup));
                }
                if t.from_files > 0 {
                    parts.push(format!("{} rebuilt from files", t.from_files));
                }
                if t.orphans_removed > 0 {
                    parts.push(format!("{} orphaned links removed", t.orphans_removed));
                }
                lines.push(format!("{}: {}", t.name, parts.join(", ")));
            }
        }
        lines.join("\n")
    }
}

const CACHE_TABLES: &[&str] = &["cover_cache", "game_metadata", "wikimedia_match"];

#[derive(Debug, Clone)]
struct Column {
    name: String,
    decl: String,
    not_null: bool,
    pk: bool,
}

fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<Column>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name,type,\"notnull\",pk FROM {schema}.pragma_table_info(?)"
    ))?;
    let cols = stmt
        .query_map([table], |r| {
            Ok(Column {
                name: r.get(0)?,
                decl: r.get::<_, String>(1)?.to_ascii_uppercase(),
                not_null: r.get::<_, i64>(2)? != 0,
                pk: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Tables (and their roots) named by integrity-check findings: tree root pages, index names
/// and `T.column` value findings.
fn damaged_tables(source: &Connection, findings: &[String]) -> HashSet<String> {
    let mut by_root: HashMap<i64, String> = HashMap::new();
    let mut by_name: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) =
        source.prepare("SELECT name,tbl_name,rootpage FROM sqlite_master WHERE rootpage>0")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) {
            for (name, table, root) in rows.flatten() {
                by_root.insert(root, table.clone());
                by_name.insert(name, table);
            }
        }
    }
    let tree = regex::Regex::new(r"Tree (\d+) page").unwrap();
    let index = regex::Regex::new(r"index (\S+)").unwrap();
    let value = regex::Regex::new(r"value in (\w+)\.").unwrap();
    let mut out = HashSet::new();
    for f in findings {
        for c in tree.captures_iter(f) {
            if let Some(t) = c[1].parse().ok().and_then(|r: i64| by_root.get(&r)) {
                out.insert(t.clone());
            }
        }
        for c in index.captures_iter(f).chain(value.captures_iter(f)) {
            if let Some(t) = by_name.get(&c[1]) {
                out.insert(t.clone());
            }
        }
    }
    out
}

/// A value fits its column: the declared type's affinity and NOT NULL / primary key.
fn fits(col: &Column, v: &ValueRef) -> bool {
    match v {
        ValueRef::Null => !(col.not_null || col.pk),
        ValueRef::Text(t) => {
            std::str::from_utf8(t).is_ok()
                && !(col.decl.contains("INT")
                    || col.decl.contains("REAL")
                    || col.decl.contains("FLOA")
                    || col.decl.contains("DOUB"))
        }
        ValueRef::Integer(_) => {
            !(col.decl.contains("CHAR") || col.decl.contains("CLOB") || col.decl.contains("TEXT"))
        }
        ValueRef::Real(_) => {
            !(col.decl.contains("INT")
                || col.decl.contains("CHAR")
                || col.decl.contains("CLOB")
                || col.decl.contains("TEXT"))
        }
        ValueRef::Blob(_) => {
            !(col.decl.contains("INT")
                || col.decl.contains("CHAR")
                || col.decl.contains("CLOB")
                || col.decl.contains("TEXT")
                || col.decl.contains("REAL"))
        }
    }
}

type Row = Vec<Value>;

fn read_row(r: &rusqlite::Row, cols: &[Column]) -> rusqlite::Result<Option<Row>> {
    let mut out = Vec::with_capacity(cols.len());
    for (i, col) in cols.iter().enumerate() {
        let v = r.get_ref(i)?;
        if !fits(col, &v) {
            return Ok(None);
        }
        out.push(Value::from(v));
    }
    Ok(Some(out))
}

/// Table-specific checks on top of the column types.
fn plausible(table: &str, names: &[String], row: &Row, game_ids: &HashSet<String>) -> bool {
    let get = |name: &str| names.iter().position(|n| n == name).map(|i| &row[i]);
    let text = |name: &str| match get(name) {
        Some(Value::Text(s)) => Some(s.as_str()),
        _ => None,
    };
    match table {
        "cover_cache" => {
            text("url").is_some_and(|u| u.starts_with("https://"))
                && text("file").is_some_and(crate::covercache::is_cache_file_name)
        }
        "game_metadata" => {
            text("gameId").is_some_and(|g| game_ids.contains(g))
                && match get("json") {
                    Some(Value::Text(j)) => serde_json::from_str::<serde_json::Value>(j).is_ok(),
                    Some(Value::Null) | None => true,
                    _ => false,
                }
        }
        "game_screenshots" => {
            let (Some(game), Some(file), Some(hash)) = (text("gameId"), text("file"), text("hash"))
            else {
                return false;
            };
            crate::screenshots::valid_relative(file).is_some_and(|(dir, name)| {
                dir == crate::screenshots::dir_for(game) && name.starts_with(hash)
            }) && hash.len() == 32
        }
        _ => true,
    }
}

struct Read {
    rows: Vec<Row>,
    rejected: usize,
    unreadable: usize,
    /// The table read cleanly and every row validated.
    clean: bool,
}

/// Every row of a table in one scan; stops at the first read error.
fn scan(source: &Connection, table: &str, cols: &[Column]) -> (Vec<(i64, Option<Row>)>, bool) {
    let list = cols
        .iter()
        .map(|c| quote(&c.name))
        .collect::<Vec<_>>()
        .join(",");
    let mut out = Vec::new();
    let Ok(mut stmt) = source.prepare(&format!("SELECT rowid,{list} FROM main.{}", quote(table)))
    else {
        return (out, false);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return (out, false);
    };
    loop {
        match rows.next() {
            Ok(Some(r)) => {
                let Ok(rowid) = r.get::<_, i64>(0) else {
                    return (out, false);
                };
                let mut values = Vec::with_capacity(cols.len());
                let mut ok = true;
                for (i, col) in cols.iter().enumerate() {
                    match r.get_ref(i + 1) {
                        Ok(v) if fits(col, &v) => values.push(Value::from(v)),
                        Ok(_) => ok = false,
                        Err(_) => return (out, false),
                    }
                }
                out.push((rowid, ok.then_some(values)));
            }
            Ok(None) => return (out, true),
            Err(_) => return (out, false),
        }
    }
}

/// Rows of a damaged table, probed one rowid at a time (a b-tree search per row, so pages
/// cross-linked into the table from elsewhere are not walked), then completed with rows the
/// scan found under rowids the search could not reach.
fn salvage(source: &Connection, table: &str, cols: &[Column]) -> Read {
    let (scanned, _) = scan(source, table, cols);
    let list = cols
        .iter()
        .map(|c| quote(&c.name))
        .collect::<Vec<_>>()
        .join(",");
    let max_seen = scanned.iter().map(|(id, _)| *id).max().unwrap_or(0);
    let max_rowid: i64 = source
        .query_row(
            &format!("SELECT max(rowid) FROM main.{}", quote(table)),
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or(0);
    let upper = max_seen
        .max(max_rowid)
        .saturating_add(PROBE_MARGIN)
        .min(MAX_PROBES);
    let mut found: BTreeMap<i64, Option<Row>> = BTreeMap::new();
    let mut unreadable = 0;
    if let Ok(mut stmt) = source.prepare(&format!(
        "SELECT {list} FROM main.{} WHERE rowid=?",
        quote(table)
    )) {
        let mut errors = HashSet::new();
        for rowid in 1..=upper {
            let mut rows = match stmt.query([rowid]) {
                Ok(rows) => rows,
                Err(_) => {
                    errors.insert(rowid);
                    continue;
                }
            };
            match rows.next() {
                Ok(Some(r)) => match read_row(r, cols) {
                    Ok(row) => {
                        found.insert(rowid, row);
                    }
                    Err(_) => {
                        errors.insert(rowid);
                    }
                },
                Ok(None) => {}
                Err(_) => {
                    errors.insert(rowid);
                }
            }
        }
        // Rowids the scan saw but the search could not reach (out-of-order keys).
        for (rowid, row) in &scanned {
            if !found.contains_key(rowid) {
                errors.remove(rowid);
                found.insert(*rowid, row.clone());
            }
        }
        unreadable = errors.len();
    } else {
        for (rowid, row) in scanned {
            found.entry(rowid).or_insert(row);
        }
    }
    let rejected = found.values().filter(|r| r.is_none()).count();
    Read {
        rows: found.into_values().flatten().collect(),
        rejected,
        unreadable,
        clean: false,
    }
}

fn read_table(source: &Connection, table: &str, cols: &[Column], damaged: bool) -> Read {
    if !damaged {
        let (scanned, complete) = scan(source, table, cols);
        let ids: HashSet<i64> = scanned.iter().map(|(id, _)| *id).collect();
        let valid = scanned.iter().all(|(_, r)| r.is_some());
        if complete && valid && ids.len() == scanned.len() {
            return Read {
                rows: scanned.into_iter().filter_map(|(_, r)| r).collect(),
                rejected: 0,
                unreadable: 0,
                clean: true,
            };
        }
    }
    salvage(source, table, cols)
}

fn insert_sql(table: &str, names: &[String]) -> String {
    format!(
        "INSERT INTO main.{}({}) VALUES({})",
        quote(table),
        names.iter().map(|n| quote(n)).collect::<Vec<_>>().join(","),
        vec!["?"; names.len()].join(",")
    )
}

/// Inserts rows; rows breaking a constraint (duplicates of a unique key) are counted, not
/// fatal.
fn insert_rows(
    dest: &Connection,
    table: &str,
    names: &[String],
    rows: &[Row],
) -> Result<(usize, usize)> {
    let mut stmt = dest.prepare(&insert_sql(table, names))?;
    let (mut ok, mut rejected) = (0, 0);
    for row in rows {
        match stmt.execute(rusqlite::params_from_iter(row.iter())) {
            Ok(_) => ok += 1,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                rejected += 1
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok((ok, rejected))
}

/// Builds a repaired copy of `source` at `dest` (which must not exist). The source is opened
/// read-only and never changed. See the module documentation.
pub fn repair_library(source: &Path, dest: &Path, options: &RepairOptions) -> Result<RepairReport> {
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    let src = open_source(source, options.ignore_wal)?;
    let result = build(&src, dest, options);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut p = dest.as_os_str().to_owned();
            p.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
    }
    result
}

fn build(src: &Connection, dest_path: &Path, options: &RepairOptions) -> Result<RepairReport> {
    let mut report = RepairReport {
        source_findings: integrity_check(src),
        ..Default::default()
    };
    let named = damaged_tables(src, &report.source_findings);
    let dest =
        Connection::open(dest_path).with_context(|| format!("creating {}", dest_path.display()))?;
    crate::db::create_schema(&dest)?;
    dest.pragma_update(None, "foreign_keys", "OFF")?;
    // Triggers (such as the one recording when a game was added) would rewrite copied rows.
    let triggers: Vec<(String, String)> = dest
        .prepare("SELECT name,sql FROM sqlite_master WHERE type='trigger'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let tx = dest.unchecked_transaction()?;
    for (name, _) in &triggers {
        tx.execute_batch(&format!("DROP TRIGGER {}", quote(name)))?;
    }
    // Rows the schema set-up added (default settings) give way to the library's own.
    let dest_tables: Vec<String> = tx
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY rowid")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for t in &dest_tables {
        tx.execute_batch(&format!("DELETE FROM {}", quote(t)))?;
    }
    // Tables only a newer version knows are kept with their own definition.
    let src_tables: Vec<(String, String)> = src
        .prepare("SELECT name,sql FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND sql IS NOT NULL ORDER BY rowid")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    let mut order = dest_tables.clone();
    for (name, sql) in &src_tables {
        if !order.contains(name) && tx.execute_batch(sql).is_ok() {
            order.push(name.clone());
        }
    }
    let src_names: HashSet<&String> = src_tables.iter().map(|(n, _)| n).collect();
    // games first: other tables' checks look game ids up.
    order.sort_by_key(|t| t != "games");
    let mut game_ids: HashSet<String> = HashSet::new();
    for table in &order {
        let mut t = TableReport {
            name: table.clone(),
            ..Default::default()
        };
        t.in_source = src_names.contains(table);
        if !t.in_source {
            report.tables.push(t);
            continue;
        }
        if options.drop_caches && CACHE_TABLES.contains(&table.as_str()) {
            t.dropped = true;
            t.damaged = named.contains(table);
            report.tables.push(t);
            continue;
        }
        let dest_cols = columns(&tx, "main", table)?;
        let src_cols = columns(src, "main", table).unwrap_or_default();
        let shared: Vec<Column> = src_cols
            .into_iter()
            .filter(|c| dest_cols.iter().any(|d| d.name == c.name))
            .collect();
        if shared.is_empty() {
            t.damaged = true;
            report.tables.push(t);
            continue;
        }
        let names: Vec<String> = shared.iter().map(|c| c.name.clone()).collect();
        let read = read_table(src, table, &shared, named.contains(table));
        t.damaged = !read.clean;
        t.unreadable = read.unreadable;
        t.rejected = read.rejected;
        // Rows salvaged from a damaged table also get the table-specific checks; a healthy
        // table is copied exactly as it is.
        let total = read.rows.len();
        let rows: Vec<Row> = if read.clean {
            read.rows
        } else {
            read.rows
                .into_iter()
                .filter(|r| plausible(table, &names, r, &game_ids))
                .collect()
        };
        t.rejected += total - rows.len();
        let (ok, dupes) = insert_rows(&tx, table, &names, &rows)?;
        t.copied = ok;
        t.rejected += dupes;
        if table == "games" {
            game_ids = tx
                .prepare("SELECT id FROM games")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
        }
        report.tables.push(t);
    }
    if let Some(i) = report
        .tables
        .iter()
        .position(|t| t.name == "game_screenshots")
    {
        if let Some(path) = &options.known_good {
            report.tables[i].from_backup =
                merge_screenshots(&tx, path, options.screenshots_dir.as_deref())?;
        }
        if let Some(dir) = &options.screenshots_dir {
            report.tables[i].from_files = screenshots_from_files(&tx, dir)?;
        }
    }
    // Links to rows that were lost (for example a session's screenshot) are removed.
    let violations: Vec<(String, i64)> = tx
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (table, rowid) in violations {
        tx.execute(
            &format!("DELETE FROM {} WHERE rowid=?", quote(&table)),
            [rowid],
        )?;
        if let Some(t) = report.tables.iter_mut().find(|t| t.name == table) {
            t.orphans_removed += 1;
            t.copied = t.copied.saturating_sub(1);
        }
    }
    for (_, sql) in &triggers {
        tx.execute_batch(sql)?;
    }
    let version: i64 = src
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    if version > crate::db::SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", version)?;
    }
    tx.commit()?;
    dest.pragma_update(None, "foreign_keys", "ON")?;
    report.integrity = integrity_check(&dest);
    // Leave a single self-contained file.
    dest.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    drop(dest);
    Ok(report)
}

/// Screenshot rows of a known-good backup whose game still exists, whose file is still there
/// (when the folder is known) and that the repaired library doesn't have yet.
fn merge_screenshots(dest: &Connection, backup: &Path, dir: Option<&Path>) -> Result<usize> {
    let good = open_source(backup, false)?;
    if !integrity_check(&good).is_empty() {
        bail!("the known-good backup {} is damaged too", backup.display());
    }
    let has_table: bool = good.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='game_screenshots'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !has_table {
        return Ok(0);
    }
    let cols = columns(&good, "main", "game_screenshots")?;
    let dest_cols = columns(dest, "main", "game_screenshots")?;
    let shared: Vec<Column> = cols
        .into_iter()
        .filter(|c| dest_cols.iter().any(|d| d.name == c.name))
        .collect();
    let names: Vec<String> = shared.iter().map(|c| c.name.clone()).collect();
    let game_ids: HashSet<String> = dest
        .prepare("SELECT id FROM games")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (rows, _) = scan(&good, "game_screenshots", &shared);
    let file_at = names.iter().position(|n| n == "file");
    let mut added = 0;
    let mut stmt = dest
        .prepare(&insert_sql("game_screenshots", &names).replace("INSERT", "INSERT OR IGNORE"))?;
    for row in rows.into_iter().filter_map(|(_, r)| r) {
        if !plausible("game_screenshots", &names, &row, &game_ids) {
            continue;
        }
        let gid = names.iter().position(|n| n == "gameId").map(|i| &row[i]);
        if !matches!(gid, Some(Value::Text(g)) if game_ids.contains(g)) {
            continue;
        }
        if let (Some(dir), Some(i)) = (dir, file_at) {
            let Value::Text(rel) = &row[i] else { continue };
            if !dir.join(rel).is_file() {
                continue;
            }
        }
        added += stmt.execute(rusqlite::params_from_iter(row.iter()))?;
    }
    Ok(added)
}

/// Rows for screenshot files no row refers to: `<sha256(game id)[..16]>/<hash>.<ext>` maps
/// back to a game of the library; the size is read from the file and the date is the file's.
fn screenshots_from_files(dest: &Connection, root: &Path) -> Result<usize> {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return Ok(0);
    };
    let by_dir: HashMap<String, String> = dest
        .prepare("SELECT id FROM games")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|id| (crate::screenshots::dir_for(&id), id))
        .collect();
    let known: HashSet<String> = dest
        .prepare("SELECT file FROM game_screenshots")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut added = 0;
    let mut entries: Vec<(String, String, PathBuf)> = Vec::new();
    for dir in dirs.flatten() {
        let dir_name = dir.file_name().to_string_lossy().into_owned();
        if !dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let rel = format!("{dir_name}/{name}");
            if crate::screenshots::valid_relative(&rel).is_some() && !known.contains(&rel) {
                entries.push((dir_name.clone(), name, f.path()));
            }
        }
    }
    entries.sort();
    for (dir_name, name, path) in entries {
        let Some(game_id) = by_dir.get(&dir_name) else {
            continue;
        };
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() && m.len() <= crate::screenshots::MAX_SCREENSHOT_BYTES => m,
            _ => continue,
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(picture) = crate::screenshots::prepare(bytes) else {
            continue;
        };
        let hash = name.split('.').next().unwrap_or_default().to_string();
        let added_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or_else(crate::types::now_ms);
        added += dest.execute(
            "INSERT OR IGNORE INTO game_screenshots(id,gameId,file,hash,width,height,addedAt,source) VALUES(?,?,?,?,?,?,?,'user')",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                game_id,
                format!("{dir_name}/{name}"),
                hash,
                picture.width,
                picture.height,
                added_at,
            ],
        )?;
    }
    Ok(added)
}

// ---------------------------------------------------------------------------
// Start-up

/// What the start-up check did to a damaged library.
#[derive(Debug, Clone)]
pub struct RepairNotice {
    /// The damaged library was replaced by the repaired one.
    pub replaced: bool,
    /// Copy of the damaged library made with the backup API (`library.sqlite.damaged-<stamp>`).
    pub damaged_copy: Option<PathBuf>,
    /// The damaged files themselves, moved aside (`library.sqlite.damaged-<stamp>.raw` plus
    /// its `-wal` and `-shm`).
    pub damaged_raw: Option<PathBuf>,
    /// The repaired file when it was not swapped in.
    pub repaired: Option<PathBuf>,
    pub findings: Vec<String>,
    pub summary: String,
    pub error: Option<String>,
}

impl RepairNotice {
    /// The text shown to the user.
    pub fn message(&self) -> String {
        let mut text = String::from("Game Lobby found damage in its library database.\n\n");
        if self.replaced {
            text.push_str(
                "It was repaired automatically and the repaired library is now in use.\n",
            );
            if !self.summary.is_empty() {
                text.push_str("\nWhat changed:\n");
                text.push_str(&self.summary);
                text.push('\n');
            }
        } else {
            text.push_str("It could not be repaired automatically, so the library is used as it is. Some games, covers or screenshots may be missing or fail to save.\n");
            if !self.findings.is_empty() {
                text.push_str("\nFound: ");
                text.push_str(
                    &self
                        .findings
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; "),
                );
                text.push('\n');
            }
            if let Some(e) = &self.error {
                text.push_str(&format!("\nReason: {e}\n"));
            }
            if let Some(p) = &self.repaired {
                text.push_str(&format!(
                    "\nA repaired copy was saved as {}.\n",
                    p.display()
                ));
            }
        }
        if let Some(p) = &self.damaged_copy {
            text.push_str(&format!(
                "\nA copy of the damaged library was kept as {}.",
                p.display()
            ));
        }
        if let Some(p) = &self.damaged_raw {
            text.push_str(&format!(
                "\nThe damaged files were kept as {}.",
                p.display()
            ));
        }
        text
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Opens `file` after a `quick_check`. A healthy library (or a new one) is returned as is. A
/// damaged one is copied with the backup API, repaired into a new file and, when the result
/// is healthy, swapped in: the damaged main file, `-wal` and `-shm` are renamed together (so
/// they still open as a pair) before the repaired file takes the library's name. Nothing is
/// swapped while a connection to the library is open in this process; when another process
/// holds the files the rename fails and the damaged library stays in place.
pub fn open_or_repair(
    file: &Path,
    screenshots_dir: Option<&Path>,
) -> Result<(Connection, Option<RepairNotice>)> {
    let conn = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
    let findings = quick_check(&conn);
    if findings.is_empty() {
        return Ok((conn, None));
    }
    log::error!(
        "The library failed its quick check:\n  {}",
        findings.join("\n  ")
    );
    let stamp = crate::types::now_ms();
    let name = file
        .file_name()
        .ok_or_else(|| anyhow!("the library has no file name"))?
        .to_string_lossy()
        .into_owned();
    let sibling = |suffix: String| file.with_file_name(format!("{name}{suffix}"));
    let mut notice = RepairNotice {
        replaced: false,
        damaged_copy: None,
        damaged_raw: None,
        repaired: None,
        findings,
        summary: String::new(),
        error: None,
    };
    let copy = sibling(format!(".damaged-{stamp}"));
    match backup_to(&conn, &copy) {
        Ok(()) => notice.damaged_copy = Some(copy),
        Err(e) => log::warn!("Could not copy the damaged library: {e:#}"),
    }
    let repaired = sibling(format!(".repaired-{stamp}"));
    let options = RepairOptions {
        screenshots_dir: screenshots_dir.map(Path::to_path_buf),
        ..Default::default()
    };
    let report = match repair_library(file, &repaired, &options) {
        Ok(report) => report,
        Err(e) => {
            notice.error = Some(format!("{e:#}"));
            return Ok((conn, Some(notice)));
        }
    };
    notice.summary = report.summary();
    log::info!("Library repair:\n{}", notice.summary);
    if !report.healthy() {
        notice.error = Some(format!(
            "the repaired copy failed its check ({})",
            report.integrity.join("; ")
        ));
        notice.repaired = Some(repaired);
        return Ok((conn, Some(notice)));
    }
    // No connection may be open while the files change names.
    drop(conn);
    let raw = sibling(format!(".damaged-{stamp}.raw"));
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut failed = None;
    for suffix in ["", "-wal", "-shm"] {
        let from = with_suffix(file, suffix);
        if !from.exists() {
            continue;
        }
        let to = with_suffix(&raw, suffix);
        match std::fs::rename(&from, &to) {
            Ok(()) => moved.push((from, to)),
            Err(e) => {
                failed = Some(format!("{} could not be moved aside: {e}", from.display()));
                break;
            }
        }
    }
    if failed.is_none() {
        if let Err(e) = std::fs::rename(&repaired, file) {
            failed = Some(format!(
                "the repaired library could not be put in place: {e}"
            ));
        }
    }
    if let Some(e) = failed {
        for (from, to) in moved.iter().rev() {
            let _ = std::fs::rename(to, from);
        }
        notice.error = Some(e);
        notice.repaired = Some(repaired);
        let conn = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
        return Ok((conn, Some(notice)));
    }
    notice.replaced = true;
    notice.damaged_raw = Some(raw);
    let conn = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
    Ok((conn, Some(notice)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, Db};
    use crate::types::GamePatch;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("game-lobby-repair-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const GAMES: usize = 30;
    const COVERS: usize = 600;

    fn png(seed: u8) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(4 + seed as u32, 3, image::Rgb([seed, 1, 2]));
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// A library file with games, many cover cache rows (several b-tree pages), metadata and
    /// three screenshots whose files are in `<dir>/screenshots`.
    fn library(dir: &Path) -> PathBuf {
        let file = dir.join("library.sqlite");
        let shots = dir.join("screenshots");
        let db = Db::open(&file).unwrap();
        let conn = db.lock();
        for i in 0..GAMES {
            db::save_game(
                &conn,
                &GamePatch {
                    title: Some(format!("Game {i}")),
                    store: Some("Custom".into()),
                    ..GamePatch::new(format!("custom:{i}"))
                },
            )
            .unwrap();
            conn.execute(
                "INSERT INTO game_metadata(gameId,igdbId,json,fetchedAt,source) VALUES(?,?,?,?,'manual')",
                rusqlite::params![format!("custom:{i}"), i as i64, "{\"name\":\"x\"}", 1],
            )
            .unwrap();
        }
        for i in 0..COVERS {
            conn.execute(
                "INSERT INTO cover_cache(url,file,fetchedAt) VALUES(?,?,?)",
                rusqlite::params![
                    format!("https://images.igdb.com/igdb/image/upload/t_original/{i:08}.jpg"),
                    format!("cache-{i:016x}-{i:012x}.jpg"),
                    i as i64
                ],
            )
            .unwrap();
        }
        for seed in 0..3u8 {
            let picture = crate::screenshots::prepare(png(seed)).unwrap();
            crate::screenshots::store(&conn, &shots, "custom:1", &picture, "user", None, 1000)
                .unwrap();
        }
        drop(conn);
        drop(db);
        file
    }

    fn page_size(file: &Path) -> usize {
        let bytes = std::fs::read(file).unwrap();
        match u16::from_be_bytes([bytes[16], bytes[17]]) {
            1 => 65536,
            n => n as usize,
        }
    }

    fn root(file: &Path, name: &str) -> u32 {
        let conn = open_source(file, false).unwrap();
        conn.query_row(
            "SELECT rootpage FROM sqlite_master WHERE name=?",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Overwrites page `page` (1-based) with garbage.
    fn smash(file: &Path, page: u32) {
        let size = page_size(file);
        let mut bytes = std::fs::read(file).unwrap();
        let start = (page as usize - 1) * size;
        bytes[start..start + size].fill(0xA5);
        std::fs::write(file, bytes).unwrap();
    }

    /// The first child of an interior table page.
    fn first_child(file: &Path, page: u32) -> u32 {
        let size = page_size(file);
        let bytes = std::fs::read(file).unwrap();
        let p = &bytes[(page as usize - 1) * size..page as usize * size];
        assert_eq!(p[0], 5, "page {page} is not an interior table page");
        let cell = u16::from_be_bytes([p[12], p[13]]) as usize;
        u32::from_be_bytes([p[cell], p[cell + 1], p[cell + 2], p[cell + 3]])
    }

    /// Damages one cover cache leaf and the whole screenshot table.
    fn corrupt(file: &Path) {
        let leaf = first_child(file, root(file, "cover_cache"));
        let shots = root(file, "game_screenshots");
        smash(file, leaf);
        smash(file, shots);
    }

    fn count(file: &Path, table: &str) -> i64 {
        let conn = open_source(file, false).unwrap();
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn sha(file: &Path) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(std::fs::read(file).unwrap()))
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn quick_check_tells_healthy_from_damaged() {
        let dir = TempDir::new();
        let file = library(&dir.0);
        assert!(quick_check(&open_source(&file, false).unwrap()).is_empty());
        corrupt(&file);
        assert!(!quick_check(&open_source(&file, false).unwrap()).is_empty());
    }

    #[test]
    fn repair_salvages_rows_and_rebuilds_screenshots_from_files() {
        let dir = TempDir::new();
        let file = library(&dir.0);
        let added = count(&file, "game_added");
        corrupt(&file);
        let before = sha(&file);
        let dest = dir.0.join("repaired.sqlite");
        let report = repair_library(
            &file,
            &dest,
            &RepairOptions {
                screenshots_dir: Some(dir.0.join("screenshots")),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.healthy(), "{:?}", report.integrity);
        assert!(!report.source_findings.is_empty());
        assert_eq!(sha(&file), before, "the source is never written");
        assert_eq!(count(&dest, "games"), GAMES as i64);
        assert_eq!(count(&dest, "game_metadata"), GAMES as i64);
        let covers = report.table("cover_cache").unwrap();
        assert!(covers.damaged);
        assert!(covers.copied > 0 && covers.copied < COVERS, "{covers:?}");
        assert!(covers.unreadable > 0, "{covers:?}");
        assert_eq!(count(&dest, "cover_cache"), covers.copied as i64);
        let games = report.table("games").unwrap();
        assert_eq!(games.copied, GAMES);
        assert!(!games.damaged);
        let shots = report.table("game_screenshots").unwrap();
        assert_eq!(shots.from_files, 3, "{shots:?}");
        let conn = open_source(&dest, false).unwrap();
        let ids: Vec<String> = conn
            .prepare("SELECT DISTINCT gameId FROM game_screenshots")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, vec!["custom:1".to_string()]);
        let (w, h): (i64, i64) = conn
            .query_row(
                "SELECT min(width),min(height) FROM game_screenshots",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((w, h), (4, 3));
        // The recently-added trigger did not add or rewrite rows while copying.
        assert_eq!(count(&dest, "game_added"), added);
        assert!(!report.summary().is_empty());
    }

    #[test]
    fn repair_merges_screenshot_rows_from_a_known_good_backup() {
        let dir = TempDir::new();
        let file = library(&dir.0);
        let good = dir.0.join("good.sqlite");
        backup_to(&open_source(&file, false).unwrap(), &good).unwrap();
        let ids = |f: &Path| -> Vec<String> {
            open_source(f, false)
                .unwrap()
                .prepare("SELECT id FROM game_screenshots ORDER BY id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let original = ids(&file);
        corrupt(&file);
        let dest = dir.0.join("repaired.sqlite");
        let report = repair_library(
            &file,
            &dest,
            &RepairOptions {
                screenshots_dir: Some(dir.0.join("screenshots")),
                known_good: Some(good),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.healthy());
        let shots = report.table("game_screenshots").unwrap();
        assert_eq!((shots.from_backup, shots.from_files), (3, 0));
        assert_eq!(ids(&dest), original, "the backup keeps the original ids");
    }

    #[test]
    fn caches_can_be_dropped() {
        let dir = TempDir::new();
        let file = library(&dir.0);
        corrupt(&file);
        let dest = dir.0.join("repaired.sqlite");
        let report = repair_library(
            &file,
            &dest,
            &RepairOptions {
                drop_caches: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.healthy());
        assert!(report.table("cover_cache").unwrap().dropped);
        assert_eq!(count(&dest, "cover_cache"), 0);
        assert_eq!(count(&dest, "game_metadata"), 0);
        assert_eq!(count(&dest, "games"), GAMES as i64);
    }

    #[test]
    fn a_damaged_library_is_repaired_and_swapped_in_at_start_up() {
        let dir = TempDir::new();
        let file = library(&dir.0);
        corrupt(&file);
        let (db, notice) = Db::open_checked(&file, Some(&dir.0.join("screenshots"))).unwrap();
        let notice = notice.expect("a notice");
        assert!(notice.replaced, "{notice:?}");
        assert!(notice.message().contains("repaired automatically"));
        let copy = notice.damaged_copy.clone().unwrap();
        let raw = notice.damaged_raw.clone().unwrap();
        assert!(copy.is_file() && raw.is_file());
        assert!(copy
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("library.sqlite.damaged-"));
        // The kept copies are still the damaged library.
        assert!(!quick_check(&open_source(&copy, false).unwrap()).is_empty());
        assert!(!quick_check(&open_source(&raw, false).unwrap()).is_empty());
        let conn = db.lock();
        assert!(integrity_check(&conn).is_empty());
        let games: i64 = conn
            .query_row("SELECT count(*) FROM games", [], |r| r.get(0))
            .unwrap();
        assert_eq!(games, GAMES as i64);
        let shots: i64 = conn
            .query_row("SELECT count(*) FROM game_screenshots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shots, 3);
        drop(conn);
        drop(db);
        assert!(!listing(&dir.0).iter().any(|n| n.contains("repaired-")));
        // The next start finds a healthy library.
        assert!(Db::open_checked(&file, None).unwrap().1.is_none());
    }

    #[test]
    fn a_healthy_library_is_left_alone() {
        let dir = TempDir::new();
        let file = library(&dir.0);
        let before = sha(&file);
        let files = listing(&dir.0);
        let (conn, notice) = open_or_repair(&file, None).unwrap();
        assert!(notice.is_none());
        drop(conn);
        assert_eq!(sha(&file), before);
        assert_eq!(listing(&dir.0), files);
        let (db, notice) = Db::open_checked(&file, None).unwrap();
        assert!(notice.is_none());
        drop(db);
        assert!(listing(&dir.0)
            .iter()
            .all(|n| !n.contains("damaged") && !n.contains("repaired")));
    }

    #[test]
    fn backups_include_what_is_still_in_the_wal() {
        let dir = TempDir::new();
        let file = dir.0.join("library.sqlite");
        let conn = Connection::open(&file).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(1),(2),(3);")
            .unwrap();
        // A plain file copy misses everything that is only in the WAL.
        let copied = dir.0.join("copied.sqlite");
        std::fs::copy(&file, &copied).unwrap();
        let plain = Connection::open(&copied).unwrap();
        assert!(plain
            .query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
            .is_err());
        let backup = dir.0.join("backup.sqlite");
        backup_to(&conn, &backup).unwrap();
        assert_eq!(count(&backup, "t"), 3);
        assert!(backup_to(&conn, &backup).is_err(), "never overwrites");
    }

    #[test]
    fn ignoring_the_wal_reads_the_main_file_alone() {
        let dir = TempDir::new();
        let file = dir.0.join("library.sqlite");
        let conn = Connection::open(&file).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(1);")
            .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute("INSERT INTO t VALUES(2)", []).unwrap();
        assert_eq!(count(&file, "t"), 2);
        let main_only = open_source(&file, true).unwrap();
        let n: i64 = main_only
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn the_electron_library_is_copied_through_the_backup_api() {
        let dir = TempDir::new();
        let old = dir.0.join("old");
        std::fs::create_dir_all(&old).unwrap();
        let conn = Connection::open(old.join("library.sqlite")).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(1),(2);")
            .unwrap();
        let new = dir.0.join("new");
        assert!(db::copy_library_from(&old, &new).unwrap());
        // Only the main file: no -wal or -shm copied next to it.
        assert_eq!(listing(&new), vec!["library.sqlite".to_string()]);
        assert_eq!(count(&new.join("library.sqlite"), "t"), 2);
        assert!(
            !db::copy_library_from(&old, &new).unwrap(),
            "only when missing"
        );
    }

    #[test]
    fn file_uris_escape_special_characters() {
        assert_eq!(
            file_uri(Path::new(r"C:\a b\x?#%.sqlite"), "mode=ro"),
            "file:///C:/a b/x%3f%23%25.sqlite?mode=ro"
        );
        assert_eq!(file_uri(Path::new("/tmp/x"), ""), "file:///tmp/x");
    }

    /// Repairs a copy by hand (never the live library):
    /// `GL_REPAIR_SOURCE=<copy> GL_REPAIR_DEST=<new file> [GL_REPAIR_IGNORE_WAL=1]
    /// [GL_REPAIR_SHOTS=<screenshots dir>] [GL_REPAIR_GOOD=<known-good backup>]
    /// cargo test repair_a_copy -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn repair_a_copy() {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let source = var("GL_REPAIR_SOURCE").expect("GL_REPAIR_SOURCE");
        let dest = var("GL_REPAIR_DEST").expect("GL_REPAIR_DEST");
        let options = RepairOptions {
            ignore_wal: std::env::var("GL_REPAIR_IGNORE_WAL").is_ok_and(|v| v == "1"),
            drop_caches: std::env::var("GL_REPAIR_DROP_CACHES").is_ok_and(|v| v == "1"),
            screenshots_dir: var("GL_REPAIR_SHOTS"),
            known_good: var("GL_REPAIR_GOOD"),
        };
        let report = repair_library(&source, &dest, &options).unwrap();
        println!("source findings: {}", report.source_findings.len());
        for t in &report.tables {
            println!("{t:?}");
        }
        println!("integrity: {:?}", report.integrity);
    }
}
