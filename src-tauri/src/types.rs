//! Data shapes shared with the renderer. Field names match `src/shared/types.ts`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STORES: [&str; 11] = [
    "Steam",
    "Epic Games",
    "GOG",
    "EA app",
    "Battle.net",
    "Xbox",
    "Ubisoft",
    "Emulator",
    "itch.io",
    "Amazon Games",
    "Custom",
];

/// Lenient integer handling: older libraries and backups may contain floats.
pub mod num {
    use serde::{Deserialize, Deserializer};

    pub fn opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
        let value = Option::<serde_json::Number>::deserialize(d)?;
        Ok(value.and_then(|n| {
            n.as_i64().or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.floor() as i64)
            })
        }))
    }

    pub fn req<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        opt(d).map(|v| v.unwrap_or(0))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameRoute {
    pub source_id: String,
    pub store: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    pub launch_target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_states: Option<BTreeMap<String, bool>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "num::opt"
    )]
    pub playtime_minutes: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "num::opt"
    )]
    pub last_played: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cover: Option<String>,
    pub store: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, deserialize_with = "num::req")]
    pub playtime_minutes: i64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "num::opt"
    )]
    pub playtime_seconds: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "num::opt"
    )]
    pub last_played: Option<i64>,
    pub hidden: bool,
    pub favorite: bool,
    pub routes: Vec<GameRoute>,
}

/// A partial game update (`library:save`). Missing fields keep their stored value.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct GamePatch {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub default_cover: Option<String>,
    #[serde(default)]
    pub store: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default, deserialize_with = "num::opt")]
    pub playtime_minutes: Option<i64>,
    #[serde(default, deserialize_with = "num::opt")]
    pub playtime_seconds: Option<i64>,
    #[serde(default, deserialize_with = "num::opt")]
    pub last_played: Option<i64>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub favorite: Option<bool>,
    #[serde(default)]
    pub routes: Option<Vec<GameRoute>>,
}

impl GamePatch {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Default::default()
        }
    }
}

impl From<&Game> for GamePatch {
    fn from(game: &Game) -> Self {
        Self {
            id: game.id.clone(),
            title: Some(game.title.clone()),
            cover: game.cover.clone(),
            default_cover: game.default_cover.clone(),
            store: Some(game.store.clone()),
            genre: game.genre.clone(),
            playtime_minutes: Some(game.playtime_minutes),
            playtime_seconds: game.playtime_seconds,
            last_played: game.last_played,
            hidden: Some(game.hidden),
            favorite: Some(game.favorite),
            routes: Some(game.routes.clone()),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemeTokens {
    pub name: String,
    pub bg: String,
    pub surface: String,
    pub surface2: String,
    pub accent: String,
    pub text: String,
    pub muted: String,
    pub radius: String,
    pub gap: String,
    pub font: String,
    pub hover: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_image: Option<String>,
}

impl Default for ThemeTokens {
    fn default() -> Self {
        Self {
            name: "Game Lobby Dark".into(),
            bg: "#101113".into(),
            surface: "#181a1d".into(),
            surface2: "#22252a".into(),
            accent: "#75d18a".into(),
            text: "#f2f3f4".into(),
            muted: "#969ba3".into(),
            radius: "10px".into(),
            gap: "9px".into(),
            font: "Inter, Segoe UI, sans-serif".into(),
            hover: "lift".into(),
            background_image: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(deserialize_with = "num::req")]
    pub scan_interval_hours: i64,
    #[serde(deserialize_with = "num::req")]
    pub poster_width: i64,
    pub show_badges: bool,
    pub animations: bool,
    pub tracking: bool,
    pub theme: ThemeTokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_name: Option<String>,
    /// Fetch missing covers from SteamGridDB after each library scan (when a key is set).
    #[serde(default = "default_true")]
    pub steam_grid_db_auto_fetch: bool,
    /// Library hides games without an installed route (renderer-only filter).
    #[serde(default = "default_true")]
    pub show_only_installed: bool,
    /// SteamGridDB grid styles to request; empty means any style. Values are limited to
    /// [`STEAM_GRID_DB_STYLES`] by `validation::settings_patch`.
    #[serde(default)]
    pub steam_grid_db_styles: Vec<String>,
    /// Include animated grids in SteamGridDB requests.
    #[serde(default)]
    pub steam_grid_db_animated: bool,
    /// Include adult-flagged grids in SteamGridDB requests.
    #[serde(default)]
    pub steam_grid_db_nsfw: bool,
    /// Include humor-flagged grids in SteamGridDB requests.
    #[serde(default)]
    pub steam_grid_db_humor: bool,
    /// With `tracking` on, also detect and track installed games started outside Game Lobby
    /// (Steam, Epic, desktop shortcuts). Off = only games launched from Game Lobby are tracked.
    #[serde(default = "default_true")]
    pub detect_external_launches: bool,
    /// Games that are never detected or tracked (launching them only sets `lastPlayed`).
    /// Limits are enforced by `validation::settings_patch`.
    #[serde(default)]
    pub tracking_excluded_game_ids: Vec<String>,
}

/// Grid styles accepted by the SteamGridDB API (`styles=` on `/grids`).
pub const STEAM_GRID_DB_STYLES: &[&str] =
    &["alternate", "blurred", "white_logo", "material", "no_logo"];

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scan_interval_hours: 0,
            poster_width: 172,
            show_badges: true,
            animations: true,
            tracking: true,
            theme: ThemeTokens::default(),
            theme_url: None,
            theme_name: None,
            steam_grid_db_auto_fetch: true,
            show_only_installed: true,
            steam_grid_db_styles: Vec::new(),
            steam_grid_db_animated: false,
            steam_grid_db_nsfw: false,
            steam_grid_db_humor: false,
            detect_external_launches: true,
            tracking_excluded_game_ids: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EmulatorProfile {
    pub id: String,
    pub name: String,
    pub executable: String,
    pub args: String,
    pub rom_folders: Vec<String>,
    pub extensions: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub store: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_folder: Option<String>,
}

/// Non-secret library connection values stored in `library_connections`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionValues {
    #[serde(default)]
    pub steam_id: String,
    #[serde(default)]
    pub legendary_path: String,
    #[serde(default)]
    pub gog_database_path: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LibraryConnectionInput {
    pub steam_id: String,
    #[serde(default)]
    pub steam_api_key: Option<String>,
    pub legendary_path: String,
    pub gog_database_path: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LibraryConnections {
    pub steam_id: String,
    pub steam_api_key_set: bool,
    pub legendary_path: String,
    pub gog_database_path: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStats {
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Backup {
    pub games: Vec<Game>,
    #[serde(default)]
    pub settings: Option<Settings>,
    #[serde(default)]
    pub emulators: Option<Vec<EmulatorProfile>>,
}

/// One stored play session (`tracking_sessions`). `ended_at` is absent while it is in progress.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaySession {
    pub id: i64,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub seconds: i64,
    /// `"launch"` or `"detected"`.
    pub source: String,
}

/// Playtime summary for one game (`tracking_stats`).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlayStats {
    pub total_seconds: i64,
    pub last_two_weeks_seconds: i64,
    pub session_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_played: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
}

/// Game page art candidates in priority order (`art_get`).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct GameArt {
    pub hero: Vec<String>,
    pub logo: Vec<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// JavaScript-style truthiness for optional strings: `None` and `""` are both empty.
pub fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.is_empty())
}
