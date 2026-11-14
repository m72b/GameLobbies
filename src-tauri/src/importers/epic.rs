//! Epic Games: installed games from the Epic Games Launcher `.item` manifests.

use super::{single_route_game, states};
use crate::platform::encode_uri_component;
use crate::types::{Game, GameRoute};
use anyhow::{bail, Result};
use serde_json::Value;
use std::path::Path;

const MANIFEST_ROOTS: [&str; 2] = [
    r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests",
    r"C:\ProgramData\Epic\EpicGames\Launcher\Data\Manifests",
];

/// JavaScript-style `a || b` over JSON values: the first non-empty string or number.
fn first_truthy(values: &[Option<&Value>]) -> Option<String> {
    values.iter().flatten().find_map(|v| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) if n.as_f64() != Some(0.0) => Some(n.to_string()),
        _ => None,
    })
}

pub fn launch_uri(app_name: &str, action: &str) -> String {
    format!(
        "com.epicgames.launcher://apps/{}?action={action}&silent=true",
        encode_uri_component(app_name)
    )
}

fn parse_item(manifest: &Value, file: &str) -> Result<Game> {
    let get = |key: &str| manifest.get(key);
    let app_name = first_truthy(&[
        get("AppName"),
        get("CatalogItemId"),
        get("InstallationGuid"),
    ])
    .unwrap_or_else(|| file.to_string());
    let title = first_truthy(&[get("DisplayName"), get("AppName")])
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        bail!("manifest has no display title");
    }
    let key_image = manifest.pointer("/KeyImages/0/URL");
    let cover = first_truthy(&[get("LargePreviewImage"), key_image]);
    let install_path = get("InstallLocation")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Ok(single_route_game(
        format!("epic:{app_name}"),
        title,
        "Epic Games",
        cover,
        GameRoute {
            source_id: "epic".into(),
            store: "Epic Games".into(),
            launch_target: launch_uri(&app_name, "launch"),
            app_id: Some(app_name),
            install_path,
            installed: Some(true),
            install_states: states("epic-installed", true),
            ..Default::default()
        },
    ))
}

pub fn scan_installed() -> Result<Vec<Game>> {
    let mut games = Vec::new();
    for root in MANIFEST_ROOTS.iter().map(Path::new).filter(|p| p.exists()) {
        let mut files: Vec<_> = std::fs::read_dir(root)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".item"))
            .collect();
        files.sort();
        for file in files {
            let parsed = std::fs::read_to_string(root.join(&file))
                .map_err(anyhow::Error::from)
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(anyhow::Error::from))
                .and_then(|manifest| parse_item(&manifest, &file));
            match parsed {
                Ok(game) => games.push(game),
                Err(e) => bail!("Could not verify Epic manifest {file}: {e}"),
            }
        }
    }
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_launcher_manifests() {
        let game = parse_item(&json!({ "AppName": "Fortnite Game", "DisplayName": " Fortnite ", "InstallLocation": "C:/Epic/Fortnite" }), "x.item").unwrap();
        assert_eq!(game.id, "epic:Fortnite Game");
        assert_eq!(game.title, "Fortnite");
        assert_eq!(
            game.routes[0].launch_target,
            "com.epicgames.launcher://apps/Fortnite%20Game?action=launch&silent=true"
        );
        assert!(parse_item(&json!({ "InstallLocation": "x" }), "x.item").is_err());
    }
}
