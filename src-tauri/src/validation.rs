//! Input validation for everything that crosses the renderer → Rust boundary.
//! Mirrors the zod schemas of the Electron build: unknown fields are rejected,
//! strings are trimmed where they were before, and sizes are bounded.

use crate::types::*;
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::sync::LazyLock;

static COLOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#(?:[\da-f]{3,4}|[\da-f]{6}|[\da-f]{8})$").unwrap());
static PIXELS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}px$").unwrap());
static FONT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^[\w ,"'-]{1,80}$"#).unwrap());
static EXTENSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\.[\da-z]{1,16}$").unwrap());
static PLUGIN_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9][a-z0-9._-]{1,63}$").unwrap());
static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+\.\d+\.\d+(?:[-+][\w.-]+)?$").unwrap());

const ROUTE_KEYS: &[&str] = &[
    "sourceId",
    "store",
    "appId",
    "launchTarget",
    "launchArgs",
    "installPath",
    "owned",
    "installed",
    "installStates",
    "playtimeMinutes",
    "lastPlayed",
    "emulatorId",
    "system",
];
const GAME_KEYS: &[&str] = &[
    "id",
    "title",
    "cover",
    "defaultCover",
    "store",
    "genre",
    "playtimeMinutes",
    "playtimeSeconds",
    "lastPlayed",
    "hidden",
    "favorite",
    "routes",
];
const THEME_KEYS: &[&str] = &[
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
const SETTINGS_KEYS: &[&str] = &[
    "scanIntervalHours",
    "posterWidth",
    "showBadges",
    "animations",
    "tracking",
    "theme",
    "themeUrl",
    "themeName",
    "steamGridDbAutoFetch",
    "showOnlyInstalled",
    "steamGridDbStyles",
    "steamGridDbAnimated",
    "steamGridDbNsfw",
    "steamGridDbHumor",
    "detectExternalLaunches",
    "trackingExcludedGameIds",
    "autoCheckUpdates",
    "disabledStores",
];
/// Settings keys of older versions that are dropped (not rejected) when a backup is imported.
const RETIRED_SETTINGS_KEYS: &[&str] = &["steamId", "steamApiKey", "legendaryPath"];
const EMULATOR_KEYS: &[&str] = &[
    "id",
    "name",
    "executable",
    "args",
    "romFolders",
    "extensions",
    "systems",
    "enabled",
    "source",
    "detectedAs",
    "available",
    "needsConfiguration",
    "addedAt",
];
const PLUGIN_KEYS: &[&str] = &["id", "name", "version", "kind", "store", "manifestFolder"];
const BACKUP_KEYS: &[&str] = &["games", "settings", "emulators", "gamePageCustom"];
/// Top-level backup keys of older versions (library connections and store accounts) that are
/// ignored on import.
const RETIRED_BACKUP_KEYS: &[&str] = &[
    "connections",
    "libraryConnections",
    "steamId",
    "steamApiKey",
    "legendaryPath",
];
const PAGE_CUSTOM_KEYS: &[&str] = &["gameId", "hero", "logo", "logoPosition", "logoScale"];
/// Image types accepted for custom heroes and logos (`lobby-cover://` files).
pub const PAGE_IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "avif"];
pub const LOBBY_COVER_PREFIX: &str = "lobby-cover://";
/// Bare covers-folder file names as Game Lobby creates them: ASCII letters, digits, `_.-`,
/// not starting with a dot, ending in an image extension.
static COVER_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,199}\.(png|jpe?g|webp|avif)$").unwrap()
});

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow!("{what} must be an object."))
}

/// Rejects keys outside `allowed`, like zod's `.strict()`. `null` values are dropped first
/// because JSON has no `undefined`.
fn strict(value: &Value, allowed: &[&str], what: &str) -> Result<Value> {
    let map = object(value, what)?;
    let mut clean = Map::new();
    for (key, v) in map {
        if !allowed.contains(&key.as_str()) {
            bail!("{what} has an unrecognized field \"{key}\".");
        }
        if !v.is_null() {
            clean.insert(key.clone(), v.clone());
        }
    }
    Ok(Value::Object(clean))
}

fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|e| anyhow!("{what} is invalid: {e}"))
}

fn short_text(value: &mut String, max: usize, what: &str) -> Result<()> {
    *value = value.trim().to_string();
    let len = value.chars().count();
    if len == 0 || len > max {
        bail!("{what} must be between 1 and {max} characters.");
    }
    Ok(())
}

fn max_len(value: &Option<String>, max: usize, what: &str) -> Result<()> {
    if value.as_ref().is_some_and(|v| v.chars().count() > max) {
        bail!("{what} must be at most {max} characters.");
    }
    Ok(())
}

fn range(value: Option<i64>, min: i64, max: i64, what: &str) -> Result<()> {
    if value.is_some_and(|v| v < min || v > max) {
        bail!("{what} must be between {min} and {max}.");
    }
    Ok(())
}

fn store(value: &str) -> Result<()> {
    if !STORES.contains(&value) {
        bail!("Unknown store \"{value}\".");
    }
    Ok(())
}

pub fn is_https_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|u| {
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.host().is_some()
    })
}

pub fn https_url(raw: &str) -> Result<String> {
    if !is_https_url(raw) {
        bail!("URL must use HTTPS without credentials.");
    }
    Ok(raw.to_string())
}

fn route(value: &Value) -> Result<GameRoute> {
    let mut r: GameRoute = decode(strict(value, ROUTE_KEYS, "Launch route")?, "Launch route")?;
    short_text(&mut r.source_id, 128, "Route source")?;
    store(&r.store)?;
    // ROM and local PC routes use the normalized file path as their app ID.
    max_len(&r.app_id, 8_192, "Route app ID")?;
    max_len(&r.emulator_id, 256, "Route emulator")?;
    if let Some(system) = &r.system {
        if !crate::catalog::is_system_id(system) {
            bail!("Unknown system \"{system}\".");
        }
    }
    short_text(&mut r.launch_target, 8_192, "Launch target")?;
    max_len(&r.launch_args, 8_192, "Launch arguments")?;
    max_len(&r.install_path, 8_192, "Install path")?;
    if r.install_states
        .as_ref()
        .is_some_and(|s| s.keys().any(|k| k.chars().count() > 128))
    {
        bail!("Install state keys must be at most 128 characters.");
    }
    range(r.playtime_minutes, 0, 1_000_000_000, "Route playtime")?;
    range(r.last_played, 0, 8_640_000_000_000_000, "Route last played")?;
    Ok(r)
}

fn routes(value: Option<&Value>) -> Result<Option<Vec<GameRoute>>> {
    let Some(value) = value else { return Ok(None) };
    let list = value
        .as_array()
        .ok_or_else(|| anyhow!("Routes must be a list."))?;
    if list.len() > 128 {
        bail!("A game can have at most 128 launch routes.");
    }
    list.iter().map(route).collect::<Result<_>>().map(Some)
}

/// The fields of a route that decide what a launch runs (or which file or folder it touches).
/// Empty text counts as absent, like it does at launch.
fn launch_identity(r: &GameRoute) -> [Option<&str>; 7] {
    fn text(v: &Option<String>) -> Option<&str> {
        v.as_deref().filter(|s| !s.is_empty())
    }
    [
        Some(r.source_id.as_str()),
        Some(r.launch_target.as_str()),
        text(&r.launch_args),
        text(&r.app_id),
        text(&r.install_path),
        text(&r.emulator_id),
        text(&r.system),
    ]
}

/// A route the renderer may create or edit by itself: a store link that passes
/// [`crate::launch::validate_launch_uri`], with no arguments, and not a ROM route (ROM routes
/// run an emulator with their target as a file).
fn is_renderer_uri_route(r: &GameRoute) -> bool {
    r.source_id != crate::local::ROM_SOURCE
        && r.emulator_id.as_deref().is_none_or(str::is_empty)
        && r.launch_args.as_deref().is_none_or(str::is_empty)
        && crate::launch::validate_launch_uri(&r.launch_target).is_ok()
}

/// Trust boundary for routes sent by the renderer (`library:save`).
///
/// Executable, shortcut, path and ROM routes are created only by Rust-side flows (scans,
/// `library_add_local_game` with its native picker, review resolutions, backup restore picked
/// in a native dialog). The renderer may send such a route back only unchanged: every field in
/// [`launch_identity`] must equal a route already stored for the same game. It may still drop
/// or reorder routes, change their non-launch fields (installed, playtime, ...), and add or
/// edit store-link routes ([`is_renderer_uri_route`]).
pub fn check_renderer_routes(stored: &[GameRoute], incoming: &[GameRoute]) -> Result<()> {
    for route in incoming {
        if is_renderer_uri_route(route) {
            continue;
        }
        let identity = launch_identity(route);
        if !stored.iter().any(|s| launch_identity(s) == identity) {
            bail!("Launch routes for programs and files can't be changed here. Use \"Add local game\" or scan your libraries instead.");
        }
    }
    Ok(())
}

/// `library:save` — a partial game.
pub fn game_patch(value: &Value) -> Result<GamePatch> {
    let clean = strict(value, GAME_KEYS, "Game")?;
    let routes = routes(clean.get("routes"))?;
    let mut without_routes = clean.clone();
    without_routes.as_object_mut().unwrap().remove("routes");
    let mut g: GamePatch = decode(without_routes, "Game")?;
    g.routes = routes;
    short_text(&mut g.id, 512, "Game ID")?;
    if let Some(title) = g.title.as_mut() {
        short_text(title, 300, "Title")?;
    }
    if let Some(s) = &g.store {
        store(s)?;
    }
    max_len(&g.cover, 8_192, "Cover")?;
    max_len(&g.default_cover, 8_192, "Default cover")?;
    max_len(&g.genre, 500, "Genre")?;
    range(g.playtime_minutes, 0, 1_000_000_000, "Playtime")?;
    range(g.playtime_seconds, 0, 60_000_000_000, "Playtime")?;
    range(g.last_played, 0, 8_640_000_000_000_000, "Last played")?;
    Ok(g)
}

/// A complete game (backup files).
pub fn game(value: &Value) -> Result<Game> {
    let map = object(value, "Game")?;
    for key in [
        "id",
        "title",
        "store",
        "playtimeMinutes",
        "hidden",
        "favorite",
        "routes",
    ] {
        if map.get(key).is_none_or(Value::is_null) {
            bail!("Game is missing \"{key}\".");
        }
    }
    let p = game_patch(value)?;
    Ok(Game {
        id: p.id,
        title: p.title.unwrap_or_default(),
        cover: p.cover,
        default_cover: p.default_cover,
        store: p.store.unwrap_or_default(),
        genre: p.genre,
        playtime_minutes: p.playtime_minutes.unwrap_or(0),
        playtime_seconds: p.playtime_seconds,
        last_played: p.last_played,
        hidden: p.hidden.unwrap_or(false),
        favorite: p.favorite.unwrap_or(false),
        routes: p.routes.unwrap_or_default(),
    })
}

pub fn theme(value: &Value) -> Result<ThemeTokens> {
    let mut t: ThemeTokens = decode(strict(value, THEME_KEYS, "Theme")?, "Theme")?;
    short_text(&mut t.name, 80, "Theme name")?;
    for color in [&t.bg, &t.surface, &t.surface2, &t.accent, &t.text, &t.muted] {
        if !COLOR.is_match(color) {
            bail!("Theme color \"{color}\" is not a hex color.");
        }
    }
    if !PIXELS.is_match(&t.radius) || !PIXELS.is_match(&t.gap) {
        bail!("Theme radius and gap must be pixel values.");
    }
    if !FONT.is_match(&t.font) {
        bail!("Theme font is invalid.");
    }
    if !["lift", "glow", "none"].contains(&t.hover.as_str()) {
        bail!("Theme hover must be lift, glow, or none.");
    }
    if let Some(image) = &t.background_image {
        https_url(image)?;
    }
    Ok(t)
}

fn int_field(map: &Map<String, Value>, key: &str, min: i64, max: i64) -> Result<()> {
    if let Some(v) = map.get(key) {
        let n = v
            .as_i64()
            .ok_or_else(|| anyhow!("{key} must be a whole number."))?;
        range(Some(n), min, max, key)?;
    }
    Ok(())
}

fn bool_field(map: &Map<String, Value>, key: &str) -> Result<()> {
    if map.get(key).is_some_and(|v| !v.is_boolean()) {
        bail!("{key} must be true or false.");
    }
    Ok(())
}

pub const MAX_EXCLUDED_GAMES: usize = 5_000;
pub const MAX_GAME_ID_LEN: usize = 512;

/// `trackingExcludedGameIds`: distinct, non-empty game ids of at most [`MAX_GAME_ID_LEN`]
/// characters, at most [`MAX_EXCLUDED_GAMES`] of them.
fn excluded_game_ids(value: &Value) -> Result<()> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("trackingExcludedGameIds must be a list."))?;
    if items.len() > MAX_EXCLUDED_GAMES {
        bail!("trackingExcludedGameIds may list at most {MAX_EXCLUDED_GAMES} games.");
    }
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let id = item
            .as_str()
            .filter(|id| !id.is_empty() && id.chars().count() <= MAX_GAME_ID_LEN)
            .ok_or_else(|| {
                anyhow!("trackingExcludedGameIds must contain game ids of up to {MAX_GAME_ID_LEN} characters.")
            })?;
        if !seen.insert(id) {
            bail!("trackingExcludedGameIds lists a game more than once.");
        }
    }
    Ok(())
}

/// `steamGridDbStyles`: an array of distinct known SteamGridDB styles (empty = any style).
fn grid_styles(value: &Value) -> Result<()> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("steamGridDbStyles must be a list."))?;
    let mut seen: Vec<&str> = Vec::new();
    for item in items {
        let style = item
            .as_str()
            .filter(|s| STEAM_GRID_DB_STYLES.contains(s))
            .ok_or_else(|| {
                anyhow!(
                    "steamGridDbStyles may only contain: {}.",
                    STEAM_GRID_DB_STYLES.join(", ")
                )
            })?;
        if seen.contains(&style) {
            bail!("steamGridDbStyles lists \"{style}\" more than once.");
        }
        seen.push(style);
    }
    Ok(())
}

/// `settings:save` — returns a JSON patch for `db::put_settings`.
pub fn settings_patch(value: &Value) -> Result<Map<String, Value>> {
    let Value::Object(mut map) = strict(value, SETTINGS_KEYS, "Settings")? else {
        unreachable!()
    };
    int_field(&map, "scanIntervalHours", 0, 168)?;
    int_field(&map, "posterWidth", 120, 250)?;
    for key in [
        "showBadges",
        "animations",
        "tracking",
        "steamGridDbAutoFetch",
        "showOnlyInstalled",
        "steamGridDbAnimated",
        "steamGridDbNsfw",
        "steamGridDbHumor",
        "detectExternalLaunches",
        "autoCheckUpdates",
    ] {
        bool_field(&map, key)?;
    }
    if let Some(v) = map.get("steamGridDbStyles") {
        grid_styles(v)?;
    }
    if let Some(v) = map.get("trackingExcludedGameIds") {
        excluded_game_ids(v)?;
    }
    if let Some(v) = map.get("disabledStores") {
        store_ids(v)?;
    }
    if let Some(t) = map.get("theme") {
        let t = theme(t)?;
        map.insert("theme".into(), serde_json::to_value(t)?);
    }
    if let Some(v) = map.get("themeUrl") {
        https_url(
            v.as_str()
                .ok_or_else(|| anyhow!("themeUrl must be text."))?,
        )?;
    }
    if let Some(v) = map.get("themeName") {
        max_len(
            &Some(
                v.as_str()
                    .ok_or_else(|| anyhow!("themeName must be text."))?
                    .to_string(),
            ),
            80,
            "Theme name",
        )?;
    }
    Ok(map)
}

pub fn settings(value: &Value) -> Result<Settings> {
    let map = settings_patch(value)?;
    for key in [
        "scanIntervalHours",
        "posterWidth",
        "showBadges",
        "animations",
        "tracking",
        "theme",
    ] {
        if !map.contains_key(key) {
            bail!("Settings are missing \"{key}\".");
        }
    }
    decode(Value::Object(map), "Settings")
}

pub fn emulator(value: &Value) -> Result<EmulatorProfile> {
    let mut p: EmulatorProfile = decode(
        strict(value, EMULATOR_KEYS, "Emulator profile")?,
        "Emulator profile",
    )?;
    short_text(&mut p.id, 256, "Emulator ID")?;
    short_text(&mut p.name, 128, "Emulator name")?;
    short_text(&mut p.executable, 8_192, "Emulator executable")?;
    // Existence is checked at launch; the shape is checked here (saves and backups alike).
    if !crate::emulators::executable_path_ok(&p.executable) {
        bail!(
            r"The emulator executable must be a full path to an .exe file, like C:\Emulators\Dolphin\Dolphin.exe."
        );
    }
    max_len(&Some(p.args.clone()), 8_192, "Emulator arguments")?;
    if p.rom_folders.len() > 128 || p.extensions.len() > 256 {
        bail!("Too many ROM folders or extensions.");
    }
    for folder in &mut p.rom_folders {
        short_text(folder, 8_192, "ROM folder")?;
    }
    if let Some(bad) = p.extensions.iter().find(|e| !EXTENSION.is_match(e)) {
        bail!("\"{bad}\" is not a file extension like .iso.");
    }
    if p.systems.len() > 32 {
        bail!("An emulator can list at most 32 systems.");
    }
    let mut seen = std::collections::HashSet::new();
    for system in &p.systems {
        if !crate::catalog::is_system_id(system) {
            bail!("Unknown system \"{system}\".");
        }
        if !seen.insert(system) {
            bail!("System \"{system}\" is listed twice.");
        }
    }
    if !["manual", "discovered"].contains(&p.source.as_str()) {
        bail!("Emulator source must be \"manual\" or \"discovered\".");
    }
    if let Some(id) = &p.detected_as {
        if crate::catalog::known_emulator(id).is_none() {
            bail!("Unknown emulator \"{id}\".");
        }
    }
    // Output-only fields are recomputed when profiles are listed.
    p.available = true;
    p.needs_configuration = false;
    Ok(p)
}

/// `disabledStores`: distinct ids from [`STORE_SOURCES`].
fn store_ids(value: &Value) -> Result<()> {
    let list = value
        .as_array()
        .ok_or_else(|| anyhow!("disabledStores must be a list."))?;
    let mut seen = std::collections::HashSet::new();
    for item in list {
        let id = item
            .as_str()
            .filter(|id| STORE_SOURCES.iter().any(|(known, _)| known == id))
            .ok_or_else(|| anyhow!("disabledStores has an unknown store."))?;
        if !seen.insert(id) {
            bail!("disabledStores lists a store twice.");
        }
    }
    Ok(())
}

/// Removes retired keys from an object (old backups), so they are ignored rather than rejected.
fn without(value: &Value, retired: &[&str]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !retired.contains(&key.as_str()))
                .map(|(key, v)| (key.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn plugin_manifest(value: &Value) -> Result<PluginManifest> {
    let mut m: PluginManifest = decode(
        strict(value, PLUGIN_KEYS, "Plugin manifest")?,
        "Plugin manifest",
    )?;
    if !PLUGIN_ID.is_match(&m.id) {
        bail!("Plugin ID is invalid.");
    }
    short_text(&mut m.name, 128, "Plugin name")?;
    if m.version.len() > 64 || !VERSION.is_match(&m.version) {
        bail!("Plugin version must look like 1.2.3.");
    }
    if m.kind != "library" {
        bail!("Only library plugins are supported.");
    }
    store(&m.store)?;
    max_len(&m.manifest_folder, 8_192, "Manifest folder")?;
    Ok(m)
}

/// The file name in a `lobby-cover://<name>` value, when it is a safe bare covers-folder name
/// (no paths, `..`, percent-escapes, drive letters or Windows device names). Syntax only: the
/// caller checks the file exists.
pub fn lobby_cover_name(raw: &str) -> Option<&str> {
    let name = raw.strip_prefix(LOBBY_COVER_PREFIX)?;
    if !COVER_NAME.is_match(name) || name.contains("..") {
        return None;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    (!device).then_some(name)
}

/// A custom hero/logo value: a SteamGridDB https image URL (not a video) or a
/// `lobby-cover://<name>` value with a safe name. Returns the value to store.
pub fn page_art_value(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.starts_with(LOBBY_COVER_PREFIX) {
        return lobby_cover_name(raw)
            .map(|name| format!("{LOBBY_COVER_PREFIX}{name}"))
            .ok_or_else(|| anyhow!("That image file name is not valid."));
    }
    let url = crate::steamgriddb::validate_cover_url(raw)
        .map_err(|_| anyhow!("Only SteamGridDB images or your own image files can be used."))?;
    if url::Url::parse(&url).is_ok_and(|u| u.path().to_ascii_lowercase().ends_with(".webm")) {
        bail!("Videos can't be used as game page art.");
    }
    Ok(url)
}

/// Logo layout: a known position and a finite scale in [`LOGO_SCALE_MIN`, `LOGO_SCALE_MAX`].
pub fn logo_layout(position: &str, scale: f64) -> Result<(LogoPosition, f64)> {
    let position = LogoPosition::parse(position).ok_or_else(|| {
        anyhow!("Logo position must be bottom-left, top-center, center or bottom-center.")
    })?;
    if !valid_logo_scale(scale) {
        bail!("Logo size must be between {LOGO_SCALE_MIN} and {LOGO_SCALE_MAX}.");
    }
    Ok((position, scale))
}

/// One `gamePageCustom` backup entry.
pub fn page_custom_entry(value: &Value) -> Result<GamePageCustomEntry> {
    let clean = strict(value, PAGE_CUSTOM_KEYS, "Game page customization")?;
    let map = object(&clean, "Game page customization")?;
    let game_id = map
        .get("gameId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.chars().count() <= MAX_GAME_ID_LEN)
        .ok_or_else(|| anyhow!("Game page customization has an invalid gameId."))?;
    let art = |key: &str| -> Result<Option<String>> {
        match map.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => page_art_value(s)
                .map(Some)
                .map_err(|e| anyhow!("Game page customization {key}: {e}")),
            Some(_) => bail!("Game page customization {key} must be text."),
        }
    };
    let position = match map.get("logoPosition") {
        None => LogoPosition::default().as_str(),
        Some(v) => v
            .as_str()
            .ok_or_else(|| anyhow!("Game page customization logoPosition must be text."))?,
    };
    let scale = match map.get("logoScale") {
        None => DEFAULT_LOGO_SCALE,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| anyhow!("Game page customization logoScale must be a number."))?,
    };
    let (logo_position, logo_scale) = logo_layout(position, scale)?;
    Ok(GamePageCustomEntry {
        game_id: game_id.to_string(),
        hero: art("hero")?,
        logo: art("logo")?,
        logo_position,
        logo_scale,
    })
}

pub fn backup(value: &Value) -> Result<Backup> {
    let clean = strict(&without(value, RETIRED_BACKUP_KEYS), BACKUP_KEYS, "Backup")?;
    let games = clean
        .get("games")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Backup has no games list."))?;
    if games.len() > 100_000 {
        bail!("Backup has too many games.");
    }
    let games = games.iter().map(game).collect::<Result<Vec<_>>>()?;
    let settings = clean
        .get("settings")
        .map(|s| settings(&without(s, RETIRED_SETTINGS_KEYS)))
        .transpose()?;
    let emulators = match clean.get("emulators") {
        Some(Value::Array(list)) if list.len() <= 1_000 => {
            Some(list.iter().map(emulator).collect::<Result<Vec<_>>>()?)
        }
        Some(_) => bail!("Backup emulators are invalid."),
        None => None,
    };
    let game_page_custom = match clean.get("gamePageCustom") {
        Some(Value::Array(list)) if list.len() <= 100_000 => Some(
            list.iter()
                .map(page_custom_entry)
                .collect::<Result<Vec<_>>>()?,
        ),
        Some(_) => bail!("Backup game page customizations are invalid."),
        None => None,
    };
    Ok(Backup {
        games,
        settings,
        emulators,
        game_page_custom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn route() -> Value {
        json!({ "sourceId": "steam", "store": "Steam", "appId": "570", "launchTarget": "steam://run/570", "installed": true })
    }
    fn game_json() -> Value {
        json!({ "id": "steam:570", "title": "Dota 2", "store": "Steam", "playtimeMinutes": 0, "hidden": false, "favorite": false, "routes": [route()] })
    }
    fn settings_json() -> Value {
        json!({ "scanIntervalHours": 0, "posterWidth": 172, "showBadges": true, "animations": true, "tracking": true,
            "theme": { "name": "Default", "bg": "#101113", "surface": "#181a1d", "surface2": "#22252a", "accent": "#75d18a", "text": "#f2f3f4", "muted": "#969ba3", "radius": "10px", "gap": "9px", "font": "Inter, Segoe UI, sans-serif", "hover": "lift" } })
    }
    fn with(mut v: Value, key: &str, extra: Value) -> Value {
        v.as_object_mut().unwrap().insert(key.into(), extra);
        v
    }

    #[test]
    fn library_and_backup_accept_app_data_and_reject_unexpected_fields() {
        assert_eq!(
            game_patch(&with(game_json(), "favorite", json!(true)))
                .unwrap()
                .favorite,
            Some(true)
        );
        assert_eq!(game(&game_json()).unwrap().routes.len(), 1);
        assert_eq!(
            backup(&json!({ "games": [game_json()], "settings": settings_json() }))
                .unwrap()
                .games
                .len(),
            1
        );
        assert!(game_patch(&with(game_json(), "arbitrary", json!("value"))).is_err());
        assert!(
            backup(&json!({ "games": [with(game_json(), "unexpected", json!(true))] })).is_err()
        );
        assert!(
            game_patch(&json!({ "id": "x", "routes": [with(route(), "extra", json!(1))] }))
                .is_err()
        );
    }

    fn exe_route() -> GameRoute {
        GameRoute {
            source_id: "local".into(),
            store: "Custom".into(),
            app_id: Some(r"c:\games\cairn\cairn.exe".into()),
            launch_target: r"C:\Games\Cairn\Cairn.exe".into(),
            install_path: Some(r"C:\Games\Cairn".into()),
            installed: Some(true),
            ..Default::default()
        }
    }
    fn rom_route() -> GameRoute {
        GameRoute {
            source_id: "rom".into(),
            store: "Emulator".into(),
            launch_target: r"D:\Roms\GBA\Game.gba".into(),
            system: Some("gba".into()),
            emulator_id: Some("mgba".into()),
            ..Default::default()
        }
    }
    /// What the renderer sends: the game JSON as `library_list` returned it, edited.
    fn renderer_save(stored: &[GameRoute], edit: impl FnOnce(&mut Value)) -> Result<()> {
        let mut game = json!({ "id": "local:cairn", "title": "Cairn", "routes": stored });
        edit(&mut game);
        let patch = game_patch(&game)?;
        check_renderer_routes(stored, patch.routes.as_deref().unwrap_or_default())
    }

    #[test]
    fn renderer_saves_keep_program_routes_unchanged() {
        let stored = vec![exe_route(), rom_route(), {
            let mut r = exe_route();
            r.source_id = "gog".into();
            r.launch_args = Some(r#"["/play"]"#.into());
            r
        }];
        // Title, cover, favourite and non-launch route fields round-trip.
        assert!(renderer_save(&stored, |g| {
            g["title"] = json!("Cairn (2025)");
            g["favorite"] = json!(true);
            g["cover"] = json!("https://cdn2.steamgriddb.com/grid/a.png");
            g["routes"][0]["installed"] = json!(false);
            g["routes"][0]["playtimeMinutes"] = json!(5);
        })
        .is_ok());
        // Dropping and reordering routes is fine.
        assert!(renderer_save(&stored, |g| {
            let routes = g["routes"].as_array_mut().unwrap();
            routes.reverse();
            routes.pop();
        })
        .is_ok());
        // Empty text equals absent, as at launch.
        assert!(renderer_save(&stored, |g| g["routes"][0]["launchArgs"] = json!("")).is_ok());

        let calc = r"C:\Windows\System32\calc.exe";
        for (what, edit) in [
            (
                "exe target",
                Box::new(|g: &mut Value| g["routes"][0]["launchTarget"] = json!(calc))
                    as Box<dyn FnOnce(&mut Value)>,
            ),
            (
                "exe args",
                Box::new(|g: &mut Value| g["routes"][0]["launchArgs"] = json!(r#"["/c","calc"]"#)),
            ),
            (
                "args on a store program",
                Box::new(|g: &mut Value| g["routes"][2]["launchArgs"] = json!(r#"["/evil"]"#)),
            ),
            // A different sourceId once skipped the Epic-args refusal at launch.
            (
                "source",
                Box::new(|g: &mut Value| g["routes"][2]["sourceId"] = json!("custom")),
            ),
            (
                "rom file",
                Box::new(|g: &mut Value| g["routes"][1]["launchTarget"] = json!(calc)),
            ),
            (
                "emulator",
                Box::new(|g: &mut Value| g["routes"][1]["emulatorId"] = json!("other")),
            ),
            (
                "install path",
                Box::new(|g: &mut Value| g["routes"][0]["installPath"] = json!(r"C:\Windows")),
            ),
            (
                "new exe route",
                Box::new(move |g: &mut Value| {
                    g["routes"].as_array_mut().unwrap().push(
                        json!({ "sourceId": "custom", "store": "Custom", "launchTarget": calc }),
                    )
                }),
            ),
            (
                "new rom route",
                Box::new(move |g: &mut Value| {
                    g["routes"].as_array_mut().unwrap().push(json!({ "sourceId": "rom", "store": "Emulator", "launchTarget": calc, "system": "gba" }))
                }),
            ),
            (
                "shell route",
                Box::new(|g: &mut Value| {
                    g["routes"][0]["launchTarget"] = json!(r"shell:AppsFolder\Evil!App")
                }),
            ),
            (
                "file link",
                Box::new(|g: &mut Value| {
                    g["routes"][0]["launchTarget"] = json!("file:///C:/Windows/System32/calc.exe")
                }),
            ),
        ] {
            assert!(
                renderer_save(&stored, edit).is_err(),
                "{what} should be refused"
            );
        }
        // A program route can't be copied from another game: only this game's routes count.
        assert!(check_renderer_routes(&[], &[exe_route()]).is_err());
    }

    #[test]
    fn renderer_saves_may_add_and_edit_store_links() {
        let stored = vec![route_of(&route())];
        assert!(renderer_save(&stored, |g| g["routes"][0]["launchTarget"] =
            json!("steam://run/730"))
        .is_ok());
        assert!(renderer_save(&stored, |g| g["routes"].as_array_mut().unwrap().push(
            json!({ "sourceId": "custom", "store": "Custom", "launchTarget": "com.epicgames.launcher://apps/x?action=launch" })
        ))
        .is_ok());
        // ...but not with arguments, as a ROM route, or with a scheme outside the store list.
        for bad in [
            json!({ "sourceId": "custom", "store": "Custom", "launchTarget": "steam://run/1", "launchArgs": "-x" }),
            json!({ "sourceId": "rom", "store": "Emulator", "launchTarget": "steam://run/1", "system": "gba" }),
            json!({ "sourceId": "custom", "store": "Custom", "launchTarget": "steam://run/1", "emulatorId": "mgba" }),
            json!({ "sourceId": "custom", "store": "Custom", "launchTarget": "https://evil.example/x.exe" }),
            json!({ "sourceId": "custom", "store": "Custom", "launchTarget": "ms-settings:" }),
        ] {
            assert!(
                renderer_save(&stored, |g| g["routes"]
                    .as_array_mut()
                    .unwrap()
                    .push(bad.clone()))
                .is_err(),
                "{bad}"
            );
        }
    }

    fn route_of(v: &Value) -> GameRoute {
        super::route(v).unwrap()
    }

    #[test]
    fn disabled_stores_must_be_known_and_distinct() {
        assert!(settings_patch(&json!({ "disabledStores": ["steam", "xbox"] })).is_ok());
        assert!(settings_patch(&json!({ "disabledStores": [] })).is_ok());
        assert!(settings_patch(&json!({ "disabledStores": ["legendary"] })).is_err());
        assert!(settings_patch(&json!({ "disabledStores": ["gog", "gog"] })).is_err());
        assert!(settings_patch(&json!({ "disabledStores": "steam" })).is_err());
        // The retired account fields are rejected when saving settings directly.
        assert!(settings_patch(&json!({ "steamApiKey": "x" })).is_err());
    }

    #[test]
    fn settings_emulator_and_plugin_enforce_bounds_and_known_fields() {
        assert_eq!(
            settings_patch(&json!({ "posterWidth": 220, "tracking": false })).unwrap()
                ["posterWidth"],
            220
        );
        assert!(settings_patch(&json!({ "posterWidth": 999 })).is_err());
        assert!(settings_patch(&json!({ "tracking": true, "unknownSetting": true })).is_err());
        assert_eq!(
            settings_patch(&json!({ "steamGridDbAutoFetch": false })).unwrap()
                ["steamGridDbAutoFetch"],
            false
        );
        assert!(settings_patch(&json!({ "steamGridDbAutoFetch": "yes" })).is_err());
        let patch = settings_patch(&json!({
            "showOnlyInstalled": false,
            "steamGridDbStyles": ["alternate", "no_logo"],
            "steamGridDbAnimated": true,
            "steamGridDbNsfw": true,
            "steamGridDbHumor": false
        }))
        .unwrap();
        assert_eq!(patch["steamGridDbStyles"], json!(["alternate", "no_logo"]));
        assert!(settings_patch(&json!({ "steamGridDbStyles": [] })).is_ok());
        assert!(settings_patch(&json!({
            "trackingExcludedGameIds": (0..MAX_EXCLUDED_GAMES).map(|i| i.to_string()).collect::<Vec<_>>()
        }))
        .is_ok());
        assert!(settings_patch(
            &json!({ "trackingExcludedGameIds": ["x".repeat(MAX_GAME_ID_LEN)] })
        )
        .is_ok());
        assert_eq!(
            settings_patch(&json!({ "detectExternalLaunches": false })).unwrap()
                ["detectExternalLaunches"],
            false
        );
        for bad in [
            json!({ "showOnlyInstalled": 1 }),
            json!({ "steamGridDbAnimated": "true" }),
            json!({ "steamGridDbHumor": [] }),
            json!({ "detectExternalLaunches": "no" }),
            json!({ "trackingExcludedGameIds": "steam:1" }),
            json!({ "trackingExcludedGameIds": ["steam:1", "steam:1"] }),
            json!({ "trackingExcludedGameIds": [""] }),
            json!({ "trackingExcludedGameIds": [1] }),
            json!({ "trackingExcludedGameIds": ["x".repeat(MAX_GAME_ID_LEN + 1)] }),
            json!({ "trackingExcludedGameIds": (0..=MAX_EXCLUDED_GAMES).map(|i| i.to_string()).collect::<Vec<_>>() }),
            json!({ "steamGridDbStyles": "alternate" }),
            json!({ "steamGridDbStyles": ["alternate", "alternate"] }),
            json!({ "steamGridDbStyles": ["Alternate"] }),
            json!({ "steamGridDbStyles": ["photo"] }),
            json!({ "steamGridDbStyles": [1] }),
            json!({ "steamGridDbStyles": ["alternate,blurred"] }),
        ] {
            assert!(settings_patch(&bad).is_err(), "{bad} should be rejected");
        }
        let ok = emulator(&json!({ "id": "dolphin", "name": "Dolphin", "executable": r"C:\Emu\Dolphin.exe", "args": "--exec {rom}", "romFolders": ["D:/Roms"], "extensions": [".iso"] })).unwrap();
        assert_eq!(ok.extensions[0], ".iso");
        for bad in [
            "Dolphin.exe",
            r"C:\Emu\run.bat",
            r"\\server\share\emu.exe",
            r"C:\Emu\",
            "/usr/bin/emu.exe",
        ] {
            let profile = json!({ "id": "x", "name": "X", "executable": bad, "args": "", "romFolders": [], "extensions": [] });
            assert!(emulator(&profile).is_err(), "{bad}");
            assert!(
                backup(&json!({ "games": [], "emulators": [profile] })).is_err(),
                "{bad}"
            );
        }
        assert!(emulator(&json!({ "id": "x", "name": "X", "executable": "d:/emu/X.EXE", "args": "", "romFolders": [], "extensions": [] })).is_ok());
        assert!(emulator(&json!({ "id": "bad", "name": "Bad", "executable": "", "args": "", "romFolders": [], "extensions": ["iso"] })).is_err());
        let manifest = json!({ "id": "sample-library", "name": "Sample", "version": "1.2.3", "kind": "library", "store": "Custom" });
        assert_eq!(plugin_manifest(&manifest).unwrap().name, "Sample");
        assert!(plugin_manifest(&with(manifest, "id", json!("../bad"))).is_err());
    }

    #[test]
    fn strings_are_trimmed_and_stores_checked() {
        let g = game_patch(&json!({ "id": " a ", "title": "  Hi  ", "store": "Steam" })).unwrap();
        assert_eq!((g.id.as_str(), g.title.as_deref()), ("a", Some("Hi")));
        assert!(game_patch(&json!({ "id": "a", "store": "Nope" })).is_err());
        assert!(https_url("http://x.example").is_err());
        assert!(https_url("https://u:p@x.example").is_err());
    }

    #[test]
    fn backup_page_custom_entries_are_validated() {
        let entry = |extra: Value| {
            let mut v = json!({ "gameId": "steam:570" });
            for (k, val) in extra.as_object().unwrap() {
                v.as_object_mut().unwrap().insert(k.clone(), val.clone());
            }
            backup(&json!({ "games": [game_json()], "gamePageCustom": [v] }))
        };
        let minimal = entry(json!({})).unwrap().game_page_custom.unwrap();
        assert_eq!(minimal[0].custom(), GamePageCustom::default());
        let full = entry(json!({ "hero": "https://cdn2.steamgriddb.com/hero/a.png",
            "logo": "lobby-cover://logo-a-1.webp", "logoPosition": "top-center", "logoScale": 2 }))
        .unwrap()
        .game_page_custom
        .unwrap();
        assert_eq!(full[0].logo_position, LogoPosition::TopCenter);
        assert_eq!(full[0].logo_scale, 2.0);
        // null means absent, like everywhere else in backups.
        assert!(entry(json!({ "hero": null })).is_ok());
        for bad in [
            json!({ "hero": "https://evil.example/a.png" }),
            json!({ "logo": "lobby-cover://../library.sqlite" }),
            json!({ "logo": "lobby-cover://a/b.png" }),
            json!({ "hero": 5 }),
            json!({ "logoPosition": "left" }),
            json!({ "logoScale": 3 }),
            json!({ "logoScale": "1" }),
            json!({ "gameId": "" }),
            json!({ "gameId": "x".repeat(513) }),
            json!({ "unexpected": true }),
        ] {
            assert!(entry(bad.clone()).is_err(), "{bad}");
        }
        assert!(backup(&json!({ "games": [], "gamePageCustom": {} })).is_err());
        assert!(backup(&json!({ "games": [], "gamePageCustom": [1] })).is_err());
        assert!(backup(&json!({ "games": [] }))
            .unwrap()
            .game_page_custom
            .is_none());
    }
}
