//! Remote MyUI themes: HTTPS JSON or token-only CSS, size-limited, never executed.

use crate::types::ThemeTokens;
use crate::validation::is_https_url;
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;
use std::time::Duration;

pub const MAX_THEME_BYTES: usize = 64 * 1024;
const KEYS: [&str; 12] = [
    "name",
    "bg",
    "surface",
    "surface2",
    "accent",
    "text",
    "muted",
    "radius",
    "gap",
    "font",
    "hover",
    "backgroundImage",
];

static COLOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#(?:[\da-f]{3,4}|[\da-f]{6}|[\da-f]{8})$").unwrap());
static PIXELS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}px$").unwrap());
static FONT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^[\w ,"'-]{1,80}$"#).unwrap());
static ROOT_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i):root\s*\{([^}]*)\}").unwrap());
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--([\w-]+)\s*:\s*([^;]+);?").unwrap());
static DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-([a-z0-9])").unwrap());

/// Reads at most `max` bytes, failing early on a too-large declared length.
pub async fn read_limited(mut response: reqwest::Response, max: usize) -> Result<String> {
    if response
        .content_length()
        .is_some_and(|len| len as usize > max)
    {
        bail!("Theme file is too large.");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > max {
            bail!("Theme file is too large.");
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| anyhow!("Theme file must be valid UTF-8."))
}

fn css_tokens(body: &str) -> Result<Value> {
    let block = ROOT_BLOCK
        .captures(body)
        .ok_or_else(|| anyhow!("CSS theme must define tokens in :root."))?;
    let mut values = Map::new();
    for token in TOKEN.captures_iter(&block[1]) {
        // --surface-2 → surface2, --background-image → backgroundImage
        let key = DASH
            .replace_all(&token[1], |c: &regex::Captures| c[1].to_uppercase())
            .into_owned();
        values.insert(key, Value::String(token[2].trim().to_string()));
    }
    Ok(Value::Object(values))
}

fn safe_value(key: &str, value: &str) -> Option<String> {
    let ok = match key {
        "name" => return Some(value.trim().chars().take(80).collect()),
        "bg" | "surface" | "surface2" | "accent" | "text" | "muted" => COLOR.is_match(value),
        "radius" | "gap" => PIXELS.is_match(value),
        "font" => FONT.is_match(value),
        "hover" => ["lift", "glow", "none"].contains(&value),
        "backgroundImage" => is_https_url(value),
        _ => false,
    };
    ok.then(|| value.to_string())
}

/// Validates a parsed theme document and layers its safe values over the default theme.
pub fn theme_from_value(raw: Value) -> Result<ThemeTokens> {
    let map = raw
        .as_object()
        .ok_or_else(|| anyhow!("Theme file must contain an object."))?;
    let mut theme = serde_json::to_value(ThemeTokens::default())?;
    for (key, value) in map {
        if !KEYS.contains(&key.as_str()) {
            bail!("Theme has an unrecognized token \"{key}\".");
        }
        let Some(text) = value.as_str() else {
            bail!("Theme token \"{key}\" must be text.");
        };
        if key == "name" && text.chars().count() > 80 {
            bail!("Theme name must be at most 80 characters.");
        }
        if key == "hover" && !["lift", "glow", "none"].contains(&text) {
            bail!("Theme hover must be lift, glow, or none.");
        }
        if key == "backgroundImage" && url::Url::parse(text).is_err() {
            bail!("Theme background image must be a URL.");
        }
        if let Some(safe) = safe_value(key, text) {
            theme[key] = Value::String(safe);
        }
    }
    Ok(serde_json::from_value(theme)?)
}

pub fn parse_theme(url_path: &str, body: &str) -> Result<ThemeTokens> {
    let raw = if url_path.to_lowercase().ends_with(".css") {
        css_tokens(body)?
    } else {
        serde_json::from_str(body)
            .map_err(|_| anyhow!("Theme file must contain valid JSON or token-only CSS."))?
    };
    theme_from_value(raw)
}

pub async fn load_remote(raw_url: &str) -> Result<ThemeTokens> {
    if !is_https_url(raw_url) {
        bail!("Theme URLs must use HTTPS without credentials.");
    }
    let url = url::Url::parse(raw_url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| anyhow!("Theme could not be downloaded: {}", e.without_url()))?;
    if response.status().is_redirection() {
        bail!("Theme server redirected; use the final theme URL.");
    }
    if !response.status().is_success() {
        bail!("Theme server returned {}.", response.status().as_u16());
    }
    let body = read_limited(response, MAX_THEME_BYTES).await?;
    parse_theme(url.path(), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_themes_merge_over_defaults_and_drop_unsafe_values() {
        let theme = theme_from_value(
            json!({ "name": "Remote", "accent": "#abcdef", "bg": "red", "font": "Bad;font" }),
        )
        .unwrap();
        assert_eq!(theme.name, "Remote");
        assert_eq!(theme.accent, "#abcdef");
        assert_eq!(theme.bg, ThemeTokens::default().bg);
        assert_eq!(theme.font, ThemeTokens::default().font);
        assert!(theme_from_value(json!({ "script": "x" })).is_err());
        assert!(theme_from_value(json!({ "hover": "spin" })).is_err());
    }

    #[test]
    fn css_themes_read_root_tokens() {
        let css = include_str!("../../themes/example.css");
        let theme = parse_theme("/example.css", css).unwrap();
        assert_eq!(theme.surface2, "#23352a");
        assert_eq!(theme.hover, "glow");
        assert_eq!(theme.radius, "12px");
        let json = include_str!("../../themes/example.json");
        assert_eq!(
            parse_theme("/example.json", json).unwrap().name,
            "Forest Night"
        );
        assert!(parse_theme("/x.css", "body{}").is_err());
    }

    #[tokio::test]
    async fn refuses_non_https_urls() {
        let err = load_remote("http://themes.example/theme.json")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HTTPS"));
        assert!(load_remote("https://user:pw@themes.example/t.json")
            .await
            .is_err());
    }
}
