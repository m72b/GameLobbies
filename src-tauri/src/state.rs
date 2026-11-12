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
}

impl AppState {
    pub fn covers_dir(&self) -> PathBuf {
        self.data_dir.join("covers")
    }
}
