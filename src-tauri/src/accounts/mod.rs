//! Store accounts: sign-in (Steam in the browser), store web requests and first-time setup (see
//! docs/design/store-accounts.md).
//!
//! The renderer only names a store ([`AccountStore`]) and a mode ([`AccountMode`]); URLs, hosts,
//! window labels and every credential stay in Rust, and the renderer only ever receives
//! [`AccountStatus`] values. Every command here checks that it was called from the main window.
//!
//! This version signs in to Steam only (OpenID, [`steam_openid`]); the SteamID64 goes into the
//! existing owned-games import ([`crate::steam_owned`]), which keeps working as before. The other
//! stores are listed as coming later and refuse sign-in.

pub mod apply;
pub mod http;
pub mod setup;
pub mod signin;
pub mod steam_openid;

use crate::state::AppState;
use crate::types::now_ms;
use crate::windows::{require_main, MAIN};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

/// A store with an account sign-in, now or in a later version. Amazon is deliberately absent:
/// signing in would register a device on the user's shopping account.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum AccountStore {
    Steam,
    Gog,
    Battlenet,
    Humble,
    Xbox,
    Epic,
}

impl AccountStore {
    pub const ALL: [AccountStore; 6] = [
        Self::Steam,
        Self::Gog,
        Self::Battlenet,
        Self::Humble,
        Self::Xbox,
        Self::Epic,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Steam => "steam",
            Self::Gog => "gog",
            Self::Battlenet => "battlenet",
            Self::Humble => "humble",
            Self::Xbox => "xbox",
            Self::Epic => "epic",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Steam => "Steam",
            Self::Gog => "GOG",
            Self::Battlenet => "Battle.net",
            Self::Humble => "Humble",
            Self::Xbox => "Xbox",
            Self::Epic => "Epic Games",
        }
    }

    /// Whether this build can sign in to the store.
    pub fn available(self) -> bool {
        matches!(self, Self::Steam)
    }

    fn unavailable(self) -> String {
        format!(
            "Signing in to {} is not available in this version. Its installed games are still found on this PC.",
            self.name()
        )
    }

    /// `library_connections` key with this store's account settings.
    fn state_key(self) -> String {
        format!("account:{}", self.id())
    }
}

/// How Game Lobby uses a store: its account (owned games, store playtime) or only the games
/// installed on this PC.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum AccountMode {
    Account,
    Installed,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StoredAccount {
    mode: Option<AccountMode>,
}

fn stored_mode(conn: &Connection, store: AccountStore) -> Result<Option<AccountMode>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM library_connections WHERE key=?",
            [store.state_key()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored
        .and_then(|s| serde_json::from_str::<StoredAccount>(&s).ok())
        .and_then(|a| a.mode))
}

fn save_mode(conn: &Connection, store: AccountStore, mode: AccountMode) -> Result<()> {
    conn.execute(
        "INSERT INTO library_connections(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [
            store.state_key(),
            serde_json::to_string(&StoredAccount { mode: Some(mode) })?,
        ],
    )?;
    Ok(())
}

/// Drops the store's account settings (sign-out): its mode goes back to the default.
pub fn forget_mode(conn: &Connection, store: AccountStore) -> Result<()> {
    conn.execute(
        "DELETE FROM library_connections WHERE key=?",
        [store.state_key()],
    )?;
    Ok(())
}

/// Whether the Steam owned-games import is paused ("Installed files only").
pub fn steam_import_paused(conn: &Connection) -> Result<bool> {
    Ok(stored_mode(conn, AccountStore::Steam)? == Some(AccountMode::Installed))
}

/// Lifts the pause (used when the user saves Steam credentials or signs in).
pub fn resume_steam_import(conn: &Connection) -> Result<()> {
    if steam_import_paused(conn)? {
        forget_mode(conn, AccountStore::Steam)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// State and status

/// The last sign-in or account action's outcome, shown once in Settings → Accounts.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub ok: bool,
    pub message: String,
    pub at: i64,
}

/// In-memory account state (nothing here is a secret that outlives a sign-in).
#[derive(Default)]
pub struct AccountsState {
    /// The one sign-in that may run at a time.
    pub(crate) flight: Mutex<Option<signin::Flight>>,
    pub(crate) next_flight: AtomicU64,
    /// Bumped (under the scan lock) every time the Steam account is forgotten. A sign-in that
    /// started before a sign-out saves nothing.
    pub(crate) steam_generation: AtomicU64,
    /// Steam `response_nonce` values already accepted (replay protection).
    pub(crate) seen_nonces: Mutex<steam_openid::NonceCache>,
    notices: Mutex<HashMap<AccountStore, Notice>>,
    /// Set at start-up: no setup was saved and the library had no games and no folders.
    pub fresh_install: AtomicBool,
}

impl AccountsState {
    pub fn new(fresh_install: bool) -> Self {
        Self {
            fresh_install: AtomicBool::new(fresh_install),
            ..Default::default()
        }
    }

    pub(crate) fn set_notice(&self, store: AccountStore, ok: bool, message: String) {
        lock(&self.notices).insert(
            store,
            Notice {
                ok,
                message,
                at: now_ms(),
            },
        );
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The last sync of a store's account.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub ok: bool,
    pub message: String,
    pub checked_at: i64,
}

/// What the renderer learns about one store account. Never a credential: only flags.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub store: AccountStore,
    pub name: &'static str,
    /// Whether this version can sign in to the store.
    pub available: bool,
    pub mode: AccountMode,
    pub signed_in: bool,
    /// Steam: whether the Web API key is saved (the owned-games list needs it).
    pub key_set: bool,
    /// A sign-in for this store is running (Steam: waiting for the browser).
    pub signing_in: bool,
    pub last: Option<SyncStatus>,
    pub notice: Option<Notice>,
}

pub fn statuses(state: &AppState) -> Result<Vec<AccountStatus>> {
    let flight = lock(&state.accounts.flight).as_ref().map(|f| f.store);
    let notices = lock(&state.accounts.notices).clone();
    let conn = state.db.lock();
    let flags = crate::secrets::stored_flags(&conn)?;
    let steam_last = crate::steam_owned::load_import_state(&conn)?.status;
    let mut list = Vec::new();
    for store in AccountStore::ALL {
        let (signed_in, key_set, last) = match store {
            AccountStore::Steam => (
                flags.steam_owned_steam_id,
                flags.steam_owned_key,
                steam_last.as_ref().map(|s| SyncStatus {
                    ok: s.ok,
                    message: s.message.clone(),
                    checked_at: s.checked_at,
                }),
            ),
            _ => (false, false, None),
        };
        let paused = stored_mode(&conn, store)? == Some(AccountMode::Installed);
        list.push(AccountStatus {
            store,
            name: store.name(),
            available: store.available(),
            mode: if signed_in && !paused {
                AccountMode::Account
            } else {
                AccountMode::Installed
            },
            signed_in,
            key_set,
            signing_in: flight == Some(store),
            last,
            notice: notices.get(&store).cloned(),
        });
    }
    Ok(list)
}

/// Tells the main window that account statuses changed (`accounts:changed`, to `main` only).
pub fn notify(app: &AppHandle) {
    match statuses(&app.state::<AppState>()) {
        Ok(list) => {
            let _ = app.emit_to(MAIN, "accounts:changed", list);
        }
        Err(e) => log::warn!("Could not read the account statuses: {e:#}"),
    }
}

// ---------------------------------------------------------------------------
// Commands

#[tauri::command]
pub async fn accounts_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Vec<AccountStatus>> {
    require_main(&window)?;
    statuses(&state).map_err(err)
}

/// Starts the store's sign-in (Steam: in the default web browser). The outcome arrives later as
/// `accounts:changed`.
#[tauri::command]
pub async fn accounts_sign_in(
    app: AppHandle,
    window: WebviewWindow,
    store: AccountStore,
) -> CmdResult<Vec<AccountStatus>> {
    require_main(&window)?;
    if !store.available() {
        return Err(store.unavailable());
    }
    signin::start(&app, store).await?;
    statuses(&app.state::<AppState>()).map_err(err)
}

/// Stops the running sign-in, if any (closes its window or stops its loopback listener).
#[tauri::command]
pub async fn accounts_cancel_sign_in(
    app: AppHandle,
    window: WebviewWindow,
) -> CmdResult<Vec<AccountStatus>> {
    require_main(&window)?;
    signin::cancel(&app);
    statuses(&app.state::<AppState>()).map_err(err)
}

/// Forgets the store's account and removes the not-installed games it added (games you
/// favourited, played, rated or customized stay). Steam: the SteamID and the Web API key.
#[tauri::command]
pub async fn accounts_sign_out(
    app: AppHandle,
    window: WebviewWindow,
    store: AccountStore,
) -> CmdResult<Vec<AccountStatus>> {
    require_main(&window)?;
    if !store.available() {
        return Err(store.unavailable());
    }
    signin::cancel_store(&app, store);
    let state = app.state::<AppState>();
    let deleted = crate::steam_owned::remove_all(&app).await.map_err(err)?;
    state.accounts.set_notice(
        store,
        true,
        format!(
            "Signed out of {}. {} not-installed game{} removed.",
            store.name(),
            deleted,
            if deleted == 1 { "" } else { "s" }
        ),
    );
    notify(&app);
    statuses(&state).map_err(err)
}

/// Account (owned games and store playtime) or installed files only. Installed files only keeps
/// the sign-in but removes the not-installed games the account added and stops syncing.
#[tauri::command]
pub async fn accounts_set_mode(
    app: AppHandle,
    window: WebviewWindow,
    store: AccountStore,
    mode: AccountMode,
) -> CmdResult<Vec<AccountStatus>> {
    require_main(&window)?;
    if !store.available() {
        return Err(store.unavailable());
    }
    let state = app.state::<AppState>();
    match mode {
        AccountMode::Installed => {
            let _guard = state.scan_lock.lock().await;
            // Not one SQL transaction: `undo` runs its own. The pause is written first, so if the
            // removal fails the import can't add the games back, and choosing it again finishes.
            let deleted = {
                let mut conn = state.db.lock();
                save_mode(&conn, store, AccountMode::Installed).map_err(err)?;
                crate::steam_owned::undo(&mut conn).map_err(err)?
            };
            log::info!("Steam set to installed games only ({deleted} not-installed games removed)");
            state.accounts.set_notice(
                store,
                true,
                format!("{} shows installed games only.", store.name()),
            );
            let _ = app.emit("library:changed", ());
            state.tracker.library_changed();
        }
        AccountMode::Account => {
            resume_steam_import(&state.db.lock()).map_err(err)?;
            crate::steam_owned::sync_now(&app).await.map_err(err)?;
        }
    }
    notify(&app);
    statuses(&state).map_err(err)
}

/// Syncs the store's account now (ignores the scan throttle).
#[tauri::command]
pub async fn accounts_sync(
    app: AppHandle,
    window: WebviewWindow,
    store: AccountStore,
) -> CmdResult<Vec<AccountStatus>> {
    require_main(&window)?;
    if !store.available() {
        return Err(store.unavailable());
    }
    let state = app.state::<AppState>();
    let status = statuses(&state)
        .map_err(err)?
        .into_iter()
        .find(|s| s.store == store)
        .ok_or("Unknown store.")?;
    if !status.signed_in {
        return Err(format!("Sign in to {} first.", store.name()));
    }
    if !status.key_set {
        return Err("Add your Steam Web API key first (Settings → API keys).".into());
    }
    if status.mode != AccountMode::Account {
        return Err(format!(
            "{} is set to installed games only. Choose Account to sync it.",
            store.name()
        ));
    }
    if crate::steam_owned::sync_now(&app)
        .await
        .map_err(err)?
        .is_none()
    {
        return Err("Steam is turned off under Libraries, so it was not synced.".into());
    }
    notify(&app);
    statuses(&state).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_modes_are_lowercase_ids_and_amazon_is_not_one() {
        for store in AccountStore::ALL {
            assert_eq!(
                serde_json::to_value(store).unwrap(),
                serde_json::Value::String(store.id().into())
            );
            assert_eq!(
                serde_json::from_value::<AccountStore>(store.id().into()).unwrap(),
                store
            );
        }
        for bad in ["amazon", "Steam", "steam ", "", "origin"] {
            assert!(
                serde_json::from_value::<AccountStore>(bad.into()).is_err(),
                "{bad}"
            );
        }
        assert!(serde_json::from_value::<AccountMode>("account".into()).is_ok());
        assert!(serde_json::from_value::<AccountMode>("skip".into()).is_err());
        // Only Steam signs in in this version.
        let available: Vec<_> = AccountStore::ALL.iter().filter(|s| s.available()).collect();
        assert_eq!(available, [&AccountStore::Steam]);
    }

    #[test]
    fn the_steam_pause_is_stored_per_store_and_lifted_by_new_credentials() {
        let database = crate::db::Db::memory().unwrap();
        let conn = database.lock();
        assert!(!steam_import_paused(&conn).unwrap());
        save_mode(&conn, AccountStore::Steam, AccountMode::Installed).unwrap();
        assert!(steam_import_paused(&conn).unwrap());
        assert_eq!(stored_mode(&conn, AccountStore::Gog).unwrap(), None);
        resume_steam_import(&conn).unwrap();
        assert!(!steam_import_paused(&conn).unwrap());
        // A damaged row is ignored, not trusted.
        conn.execute(
            "INSERT INTO library_connections(key,value) VALUES('account:steam','{\"mode\":\"installed\",\"x\":1}')",
            [],
        )
        .unwrap();
        assert!(!steam_import_paused(&conn).unwrap());
        forget_mode(&conn, AccountStore::Steam).unwrap();
        assert_eq!(stored_mode(&conn, AccountStore::Steam).unwrap(), None);
    }

    #[test]
    fn a_paused_import_is_skipped_by_the_steam_owned_fetch() {
        // The pause check sits next to the scan throttle in steam_owned's fetch.
        let source = include_str!("../steam_owned.rs");
        let fetch = &source[source.find("async fn fetch_if_due").unwrap()..];
        let fetch = &fetch[..fetch.find("get_steam_owned_credentials").unwrap()];
        assert!(fetch.contains("crate::accounts::steam_import_paused(&conn)?"));
    }

    #[test]
    fn every_account_command_checks_for_the_main_window() {
        for (file, source) in [
            ("mod.rs", include_str!("mod.rs")),
            ("setup.rs", include_str!("setup.rs")),
        ] {
            let source = source.replace("\r\n", "\n");
            let mut rest = source.as_str();
            let mut count = 0;
            while let Some(at) = rest.find("#[tauri::command]\npub async fn") {
                rest = &rest[at + 1..];
                let body_end = rest.find("\n}\n").unwrap();
                assert!(
                    rest[..body_end].contains("require_main(&window)?;"),
                    "{file}: a command without require_main"
                );
                count += 1;
            }
            assert!(count >= 2, "{file}");
        }
    }
}
