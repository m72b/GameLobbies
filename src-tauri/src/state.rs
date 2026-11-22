use crate::db::Db;
use crate::tracker::Tracker;
use crate::types::LibraryStats;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub db: Db,
    pub data_dir: PathBuf,
    pub tracker: Tracker,
    /// Held while a library scan runs; concurrent requests wait and share its result.
    pub scan_lock: tokio::sync::Mutex<()>,
    pub last_scan: Mutex<Option<Result<LibraryStats, String>>>,
    /// Held while a SteamGridDB bulk cover fetch runs; a second run is refused.
    pub cover_fetch_lock: tokio::sync::Mutex<()>,
    pub scan_timer: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Held while library roots (emulator/ROM/PC folders) are scanned.
    pub local_scan_lock: tokio::sync::Mutex<()>,
    /// Local cover cache (background downloads, retries).
    pub covers: crate::covercache::CacheState,
    /// IGDB access token (memory only), rate limiter and fetch bookkeeping.
    pub igdb: crate::igdb::IgdbState,
    /// Wikidata / Wikipedia / Commons request pacing and fetch bookkeeping.
    pub wikimedia: crate::wikimedia::WikimediaState,
    /// Recent native-picker results that a later save may use (see `picks`).
    pub picks: crate::picks::PickedPaths,
    /// Launches waiting for a custom launcher to open (see `pending_launch`).
    pub pending_launches: crate::pending_launch::PendingLaunches,
    /// Store sign-in windows and first-time setup (see `accounts`).
    pub accounts: crate::accounts::AccountsState,
}

impl AppState {
    pub fn covers_dir(&self) -> PathBuf {
        self.data_dir.join("covers")
    }

    /// The user's screenshots (see `screenshots`); the cover cache never touches it.
    pub fn screenshots_dir(&self) -> PathBuf {
        self.data_dir.join("screenshots")
    }
}
