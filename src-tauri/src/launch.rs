//! Launching games through their store routes.

use crate::db;
use crate::platform::{resolve, shell_open, spawn_detached};
use crate::state::AppState;
use crate::types::{non_empty, now_ms, Game};
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const LAUNCH_SCHEMES: [&str; 9] = [
    "steam",
    "com.epicgames.launcher",
    "goggalaxy",
    "eaapp",
    "origin2",
    "battlenet",
    "uplay",
    "xbox",
    "ms-xbox-gamepass",
];

/// Store launch links are limited to known store schemes without credentials or ports.
pub fn validate_launch_uri(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw).map_err(|_| anyhow!("Launch link is not a valid URL."))?;
    if !LAUNCH_SCHEMES.contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("Launch links using {}: are not allowed.", url.scheme());
    }
    Ok(url.to_string())
}

/// Accepts a JSON string array, or (for older custom routes) whitespace-separated text.
pub fn parse_launch_arguments(value: Option<&str>) -> Vec<String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    if let Ok(args) = serde_json::from_str::<Vec<String>>(value) {
        return args;
    }
    value.split_whitespace().map(str::to_owned).collect()
}

pub async fn launch_game(app: &AppHandle, game: &Game, route_index: usize) -> Result<()> {
    let route = game
        .routes
        .get(route_index)
        .ok_or_else(|| anyhow!("No launch route is configured."))?;
    let state = app.state::<AppState>();
    let target = route.launch_target.as_str();
    if target.contains("://") {
        shell_open(validate_launch_uri(target)?).await?;
    } else if target.starts_with("shell:AppsFolder\\") {
        let windows_dir = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        spawn_detached(&windows_dir.join("explorer.exe"), &[target.to_string()])?;
    } else if let Some(args) = non_empty(&route.launch_args) {
        let executable = resolve(target);
        if !executable.exists() {
            bail!("Launch target was not found.");
        }
        if route.source_id == "epic" {
            let configured = db::get_connection_values(&state.db.lock())?.legendary_path;
            if configured.is_empty()
                || !executable
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&resolve(&configured).to_string_lossy())
            {
                bail!("The configured Legendary executable changed. Update it in Library settings and scan again.");
            }
        }
        spawn_detached(&executable, &parse_launch_arguments(Some(args)))?;
    } else {
        let path = resolve(target);
        if !path.exists() {
            bail!("Launch target was not found.");
        }
        shell_open(path.to_string_lossy().into_owned()).await?;
    }
    if route.installed != Some(false) {
        let settings = db::get_settings(&state.db.lock())?;
        let watching = state.tracker.watch(&state.db, &game.id, &settings);
        if !watching {
            db::mark_game_played(&state.db.lock(), &game.id, now_ms())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_links_are_limited_to_store_schemes() {
        assert_eq!(
            validate_launch_uri("steam://run/570").unwrap(),
            "steam://run/570"
        );
        assert_eq!(
            validate_launch_uri("com.epicgames.launcher://apps/example?action=launch").unwrap(),
            "com.epicgames.launcher://apps/example?action=launch"
        );
        assert!(validate_launch_uri("javascript://alert(1)").is_err());
        assert!(validate_launch_uri("https://example.com").is_err());
        assert!(validate_launch_uri("steam://user:pass@host/run/570").is_err());
        assert!(validate_launch_uri("steam://host:99/run/570").is_err());
    }

    #[test]
    fn parses_json_or_plain_arguments() {
        assert_eq!(
            parse_launch_arguments(Some(r#"["launch","My Game"]"#)),
            ["launch", "My Game"]
        );
        assert_eq!(parse_launch_arguments(Some("-a  -b")), ["-a", "-b"]);
        assert!(parse_launch_arguments(None).is_empty());
    }
}
