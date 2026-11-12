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
    /// Emulator profile assigned to a ROM route (`sourceId` `rom`). `None` = automatic: the
    /// best enabled profile for `system` is used at launch (and stored by the next scan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_id: Option<String>,
    /// Catalog system id (`catalog::SYSTEMS`) of a ROM or local PC route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
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
    /// Check for a new Game Lobby release shortly after startup, at most once a day. Updates are
    /// never installed without the user's click. Stored settings without it default to on.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
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
            auto_check_updates: true,
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
    #[serde(default)]
    pub rom_folders: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Catalog system ids this profile runs (used to pick a game's default emulator).
    #[serde(default)]
    pub systems: Vec<String>,
    /// Disabled profiles are never picked automatically and refuse to launch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `"discovered"` (found in an emulator folder) or `"manual"`.
    #[serde(default = "manual_source")]
    pub source: String,
    /// Catalog emulator id (`catalog::EMULATORS`) the executable was recognised as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_as: Option<String>,
    /// Output only: the executable exists right now.
    #[serde(default = "default_true")]
    pub available: bool,
    /// Output only: the catalog's default arguments still need editing (RetroArch's core).
    #[serde(default)]
    pub needs_configuration: bool,
    /// When the profile was first stored (ms epoch; 0 for profiles from before this field).
    /// Older profiles win ties when a default emulator is picked. Kept from backups.
    #[serde(default)]
    pub added_at: i64,
}

fn manual_source() -> String {
    "manual".into()
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
    /// Result of the library folder (emulator/ROM/PC) part of the scan, when roots exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<crate::local::ScanSummary>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Backup {
    pub games: Vec<Game>,
    #[serde(default)]
    pub settings: Option<Settings>,
    #[serde(default)]
    pub emulators: Option<Vec<EmulatorProfile>>,
    /// Per-game page customization. Optional: older backups don't have it.
    #[serde(
        default,
        rename = "gamePageCustom",
        skip_serializing_if = "Option::is_none"
    )]
    pub game_page_custom: Option<Vec<GamePageCustomEntry>>,
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

/// Game page art candidates in priority order (`art_get`), with the game's customization.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct GameArt {
    pub hero: Vec<String>,
    pub logo: Vec<String>,
    pub custom: GamePageCustom,
}

/// Where the logo sits on the game page hero.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LogoPosition {
    #[default]
    BottomLeft,
    TopCenter,
    Center,
    BottomCenter,
}

impl LogoPosition {
    pub const ALL: [LogoPosition; 4] = [
        Self::BottomLeft,
        Self::TopCenter,
        Self::Center,
        Self::BottomCenter,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::BottomLeft => "bottom-left",
            Self::TopCenter => "top-center",
            Self::Center => "center",
            Self::BottomCenter => "bottom-center",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == raw)
    }
}

pub const LOGO_SCALE_MIN: f64 = 0.5;
pub const LOGO_SCALE_MAX: f64 = 2.0;
pub const DEFAULT_LOGO_SCALE: f64 = 1.0;

pub fn valid_logo_scale(scale: f64) -> bool {
    scale.is_finite() && (LOGO_SCALE_MIN..=LOGO_SCALE_MAX).contains(&scale)
}

/// A game's page customization (table `game_page_custom`). `hero` / `logo` are a SteamGridDB
/// https URL or a `lobby-cover://<name>` file; `None` = the default art.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GamePageCustom {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hero: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    pub logo_position: LogoPosition,
    pub logo_scale: f64,
}

impl Default for GamePageCustom {
    fn default() -> Self {
        Self {
            hero: None,
            logo: None,
            logo_position: LogoPosition::default(),
            logo_scale: DEFAULT_LOGO_SCALE,
        }
    }
}

impl GamePageCustom {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// One game's customization in a backup file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GamePageCustomEntry {
    pub game_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hero: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default)]
    pub logo_position: LogoPosition,
    #[serde(default = "default_logo_scale")]
    pub logo_scale: f64,
}

fn default_logo_scale() -> f64 {
    DEFAULT_LOGO_SCALE
}

impl GamePageCustomEntry {
    pub fn new(game_id: &str, custom: &GamePageCustom) -> Self {
        Self {
            game_id: game_id.to_string(),
            hero: custom.hero.clone(),
            logo: custom.logo.clone(),
            logo_position: custom.logo_position,
            logo_scale: custom.logo_scale,
        }
    }

    pub fn custom(&self) -> GamePageCustom {
        GamePageCustom {
            hero: self.hero.clone(),
            logo: self.logo.clone(),
            logo_position: self.logo_position,
            logo_scale: self.logo_scale,
        }
    }
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
