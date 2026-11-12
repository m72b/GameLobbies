//! GOG: owned titles from the local GOG Galaxy 2 database (opened read-only).

use super::states;
use crate::types::{Game, GameRoute};
use anyhow::{bail, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn default_database_path() -> Option<String> {
    [
        r"C:\ProgramData\GOG.com\Galaxy\storage\galaxy-2.0.db",
        r"C:\Program Files (x86)\GOG Galaxy\storage\galaxy-2.0.db",
    ]
    .into_iter()
    .find(|p| Path::new(p).exists())
    .map(str::to_owned)
}

fn galaxy_executable(database: &Path) -> Option<PathBuf> {
    let roots = [
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        database
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf),
    ];
    roots
        .into_iter()
        .flatten()
        .map(|root| root.join("GOG Galaxy").join("GalaxyClient.exe"))
        .find(|p| p.exists())
}

fn title_from_value(value: Option<String>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match serde_json::from_str::<Value>(&value) {
        Ok(Value::String(s)) => s.trim().to_string(),
        Ok(Value::Object(map)) => ["title", "value"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| value.trim().to_string()),
        _ => value.trim().to_string(),
    }
}

pub fn scan(configured: &str) -> Result<Vec<Game>> {
    let file = if configured.is_empty() {
        default_database_path().unwrap_or_default()
    } else {
        configured.to_string()
    };
    if file.is_empty() || !Path::new(&file).exists() {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(
        &file,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(1500))?;
    let tables: HashSet<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !["LibraryReleases", "GamePieces", "GamePieceTypes"]
        .iter()
        .all(|t| tables.contains(*t))
    {
        bail!("The selected GOG Galaxy database does not contain the expected library tables.");
    }
    let releases: Vec<String> = db
        .prepare("SELECT DISTINCT releaseKey FROM LibraryReleases WHERE releaseKey LIKE 'gog_%'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut title_query = db.prepare(
        "SELECT GamePieces.value FROM GamePieces
         JOIN GamePieceTypes ON GamePieces.gamePieceTypeId=GamePieceTypes.id
         WHERE GamePieces.releaseKey=? AND lower(GamePieceTypes.type) IN ('originaltitle','title')
         ORDER BY CASE lower(GamePieceTypes.type) WHEN 'originaltitle' THEN 0 ELSE 1 END LIMIT 1",
    )?;
    let mut installed_paths: HashMap<String, String> = HashMap::new();
    if tables.contains("InstalledProducts") {
        let columns: HashSet<String> = db
            .prepare("SELECT name FROM pragma_table_info('InstalledProducts')")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if columns.contains("productId") {
            let path_column = ["installationPath", "installPath", "path"]
                .into_iter()
                .find(|c| columns.contains(*c));
            let sql = match path_column {
                Some(c) => {
                    format!("SELECT CAST(productId AS TEXT), \"{c}\" FROM InstalledProducts")
                }
                None => "SELECT CAST(productId AS TEXT), NULL FROM InstalledProducts".into(),
            };
            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                let (id, path) = row?;
                installed_paths.insert(id, path.unwrap_or_default());
            }
        }
    }
    let client = galaxy_executable(Path::new(&file));
    let mut games = Vec::new();
    for release in releases {
        let product_id = release.trim_start_matches("gog_").to_string();
        let raw: Option<String> = title_query
            .query_row([&release], |r| r.get::<_, rusqlite::types::Value>(0))
            .optional()?
            .and_then(|v| match v {
                rusqlite::types::Value::Text(s) => Some(s),
                _ => None,
            });
        let title = title_from_value(raw);
        if product_id.is_empty() || title.is_empty() {
            bail!("GOG release {release} has no readable title.");
        }
        let installed = installed_paths.contains_key(&product_id);
        let install_path = installed_paths
            .get(&product_id)
            .filter(|p| !p.is_empty())
            .cloned();
        let (launch_target, launch_args) = match &client {
            Some(exe) => (
                exe.to_string_lossy().into_owned(),
                Some(serde_json::to_string(&[
                    "/command=runGame".to_string(),
                    format!("/gameId={release}"),
                ])?),
            ),
            None => (
                format!(
                    "goggalaxy://openGameView/{}",
                    crate::platform::encode_uri_component(&product_id)
                ),
                None,
            ),
        };
        games.push(Game {
            id: format!("gog:{product_id}"),
            title,
            store: "GOG".into(),
            routes: vec![GameRoute {
                source_id: "gog-galaxy".into(),
                store: "GOG".into(),
                app_id: Some(product_id),
                launch_target,
                launch_args,
                install_path,
                owned: Some(true),
                installed: Some(installed),
                install_states: states("gog-galaxy", installed),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_titles_from_json_or_plain_text() {
        assert_eq!(
            title_from_value(Some(r#"{"title":" Witcher "}"#.into())),
            "Witcher"
        );
        assert_eq!(title_from_value(Some(r#""Gwent""#.into())), "Gwent");
        assert_eq!(title_from_value(Some("Plain".into())), "Plain");
        assert_eq!(title_from_value(None), "");
    }
}
