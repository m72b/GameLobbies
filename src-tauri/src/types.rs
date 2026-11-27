//! Data shapes shared with the renderer. Field names match `src/shared/types.ts`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STORES: [&str; 12] = [
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
    "Humble",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// A label only: the custom launcher (`launchers::CustomLauncher::id`) whose games folder a
    /// scan found this local or ROM route in, so "Play from" shows the launcher's name. It never
    /// changes what runs (the route launches like any local or ROM route). Set only by Rust
    /// (scans, review resolutions, backups); the renderer can't add or change it
    /// (`validation::check_renderer_routes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_id: Option<String>,
    /// Added from the game page (`launchers_assign`). On a `launcher:<id>` route: the route was
    /// made from another of the game's routes (its target is that route's file, folder or
    /// store id). On any other route: its `launcherId` label was set there (a launcher without
    /// a program). Unassigning removes exactly that route or label, and scans keep both. Set
    /// only by Rust; the renderer can't add, change or remove it
    /// (`validation::check_renderer_routes`). Older builds ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned: Option<bool>,
    /// The route's own name ("Java Edition"), shown after the store or launcher under "Play
    /// from" and Stores. Display only: it is never part of what a launch runs, never part of the
    /// route's launch identity (`validation::launch_identity`) and never part of `db::route_identity`,
    /// so renaming can't change or unlock anything. Always plain text, at most
    /// [`crate::links::MAX_ROUTE_NAME`] characters (`validation::route_name`). Older builds ignore
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
            bg: "#141415".into(),
            surface: "#19191b".into(),
            surface2: "#232325".into(),
            accent: "#75d18a".into(),
            text: "#ededed".into(),
            muted: "#85858b".into(),
            radius: "12px".into(),
            gap: "16px".into(),
            font: "\"Segoe UI Variable Text\",\"Segoe UI\",sans-serif".into(),
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
    #[serde(default)]
    pub steam_grid_db_animated: bool,
    #[serde(default)]
    pub steam_grid_db_nsfw: bool,
    #[serde(default)]
    pub steam_grid_db_humor: bool,
    /// With `tracking` on, also detect and track installed games started outside GameLobbies
    /// (Steam, Epic, desktop shortcuts). Off = only games launched from GameLobbies are tracked.
    #[serde(default = "default_true")]
    pub detect_external_launches: bool,
    /// Games that are never detected or tracked (launching them only sets `lastPlayed`).
    /// Limits are enforced by `validation::settings_patch`.
    #[serde(default)]
    pub tracking_excluded_game_ids: Vec<String>,
    /// Check for a new GameLobbies release shortly after startup, at most once a day. Updates are
    /// never installed without the user's click. Stored settings without it default to on.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    #[serde(default)]
    pub disabled_stores: Vec<String>,
    /// Minimize GameLobbies when a game is started from it. Stored settings without it default to on.
    #[serde(default = "default_true")]
    pub minimize_on_launch: bool,
    #[serde(default)]
    pub launch_on_boot: bool,
    #[serde(default)]
    pub minimize_to_tray: bool,
    /// The main window's close button hides it to the tray icon instead of exiting (see the tray module).
    #[serde(default)]
    pub close_to_tray: bool,
    /// Motion in the interface: "full" (always animate), "system" (follow Windows' animation setting), "reduced" (short
    /// fades only) or "off" (no animation). Stored settings without it follow Windows.
    #[serde(default = "default_motion")]
    pub motion: String,
    /// The built-in Wikidata / Wikipedia / Wikimedia Commons provider (no key needed). Stored
    /// settings without it default to on.
    #[serde(default = "default_true")]
    pub wikimedia_enabled: bool,
}

pub const MOTION_CHOICES: &[&str] = &["system", "full", "reduced", "off"];

fn default_motion() -> String {
    "system".to_string()
}

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
            disabled_stores: Vec::new(),
            minimize_on_launch: true,
            launch_on_boot: false,
            minimize_to_tray: false,
            close_to_tray: false,
            motion: default_motion(),
            wikimedia_enabled: true,
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
    #[serde(default)]
    pub systems: Vec<String>,
    /// Disabled profiles are never picked automatically and refuse to launch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `"discovered"` (found in an emulator folder) or `"manual"`.
    #[serde(default = "manual_source")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_as: Option<String>,
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

/// Non-secret library connection values stored in `library_connections` (key `main`).
/// Older versions also stored `steamId` and `legendaryPath` here; those are ignored when
/// read and removed by the database migration.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionValues {
    /// A GOG Galaxy database chosen in an older version; empty = auto-detect.
    #[serde(default)]
    pub gog_database_path: String,
}

pub const STORE_SOURCES: &[(&str, &str)] = &[
    ("steam", "Steam"),
    ("epic", "Epic Games"),
    ("gog", "GOG"),
    ("ea", "EA app"),
    ("battlenet", "Battle.net"),
    ("ubisoft", "Ubisoft Connect"),
    ("amazon", "Amazon Games"),
    ("itch", "itch.io"),
    ("xbox", "Xbox / Microsoft Store"),
];

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoreStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Games found by the last scan of this store; `None` before the first scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoreDetection {
    #[serde(default)]
    pub found: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub scanned_at: i64,
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
    /// Personal scores, notes, memories and controller details. Optional: older backups
    /// don't have it.
    #[serde(default, rename = "gameNotes", skip_serializing_if = "Option::is_none")]
    pub game_notes: Option<crate::notes::NotesBackup>,
    /// "Recently added" flags (when each game was first discovered, and whether it was seen).
    /// Optional: older backups don't have it.
    #[serde(default, rename = "gameAdded", skip_serializing_if = "Option::is_none")]
    pub game_added: Option<Vec<crate::storage::GameAddedEntry>>,
    /// Finished play sessions with their journal. Optional: older backups don't have it.
    #[serde(
        default,
        rename = "playSessions",
        skip_serializing_if = "Option::is_none"
    )]
    pub play_sessions: Option<Vec<crate::sessions::SessionBackup>>,
    /// Custom launchers. Optional: older backups don't have it.
    #[serde(
        default,
        rename = "customLaunchers",
        skip_serializing_if = "Option::is_none"
    )]
    pub launchers: Option<Vec<crate::launchers::CustomLauncher>>,
    /// What each linked entry brought into its game, so it can be unlinked after a restore
    /// (`links`). Optional: older backups don't have it (and older builds refuse it).
    #[serde(default, rename = "gameLinks", skip_serializing_if = "Option::is_none")]
    pub game_links: Option<Vec<crate::links::LinkRecord>>,
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    /// Ended by a crash or shutdown; the end is the last time the game was seen running.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
}

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
    /// Set when a store reports time for the game: `total_seconds` and `last_played` already
    /// count it (larger figure, newer date); the sessions and `first_played` stay tracked-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<crate::stats::StoreTime>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct GameArt {
    pub hero: Vec<String>,
    pub logo: Vec<String>,
    pub custom: GamePageCustom,
    /// Licence and credit of every Wikimedia Commons image in `hero`/`logo`, by the value
    /// listed there (`display`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credits: Vec<crate::wikimedia::ImageCredit>,
}

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
