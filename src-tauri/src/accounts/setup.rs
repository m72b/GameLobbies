//! First-time setup: what the user chose for each store, and whether the setup screen (fresh
//! install) or the "Add store accounts" banner (existing library) should show.
//!
//! Stored as the `setup` row of the `settings` table (not part of `Settings`, so it is not in
//! backups). Both the stored row and every patch from the renderer are parsed strictly: known
//! fields and enum values only; anything else is refused (patch) or ignored (stored row).

use super::{err, AccountStore, CmdResult};
use crate::state::AppState;
use crate::types::now_ms;
use crate::windows::require_main;
use anyhow::{bail, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use tauri::{State, WebviewWindow};

const SETUP_KEY: &str = "setup";
const VERSION: u32 = 1;

/// A store the setup asks about. Amazon Games is not listed (its installed games are still
/// scanned as before).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SetupStore {
    Steam,
    Epic,
    Gog,
    Xbox,
    Battlenet,
    Humble,
    Ea,
    Ubisoft,
    Itch,
}

impl SetupStore {
    pub const ALL: [SetupStore; 9] = [
        Self::Steam,
        Self::Epic,
        Self::Gog,
        Self::Xbox,
        Self::Battlenet,
        Self::Humble,
        Self::Ea,
        Self::Ubisoft,
        Self::Itch,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Steam => "Steam",
            Self::Epic => "Epic Games",
            Self::Gog => "GOG",
            Self::Xbox => "Xbox",
            Self::Battlenet => "Battle.net",
            Self::Humble => "Humble",
            Self::Ea => "EA app",
            Self::Ubisoft => "Ubisoft Connect",
            Self::Itch => "itch.io",
        }
    }

    /// The account store behind it, for stores that have (or will have) a sign-in.
    fn account(self) -> Option<AccountStore> {
        match self {
            Self::Steam => Some(AccountStore::Steam),
            Self::Epic => Some(AccountStore::Epic),
            Self::Gog => Some(AccountStore::Gog),
            Self::Xbox => Some(AccountStore::Xbox),
            Self::Battlenet => Some(AccountStore::Battlenet),
            Self::Humble => Some(AccountStore::Humble),
            Self::Ea | Self::Ubisoft | Self::Itch => None,
        }
    }

    fn sign_in(self) -> bool {
        self.account().is_some_and(AccountStore::available)
    }

    /// What the setup says about the store in this version.
    fn note(self) -> &'static str {
        match self {
            Self::Steam => "Sign in through Steam, then add a free Steam Web API key to list the games you own.",
            Self::Epic | Self::Gog | Self::Xbox | Self::Battlenet => {
                "Sign-in is coming in a later version. Installed games are found on this PC."
            }
            Self::Humble => "Sign-in is coming in a later version. Humble has no installed-games scan.",
            Self::Ea | Self::Ubisoft | Self::Itch => "Installed games are found on this PC.",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SetupChoice {
    Signin,
    Installed,
    Skip,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetupState {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_at: Option<i64>,
    #[serde(default)]
    pub choices: BTreeMap<SetupStore, SetupChoice>,
}

impl SetupState {
    fn finished(&self) -> bool {
        self.completed_at.is_some() || self.skipped_at.is_some()
    }
}

/// A change from the renderer. `completed` / `skipped` may only be `true` (the time is set here).
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetupPatch {
    #[serde(default)]
    pub choices: Option<BTreeMap<SetupStore, SetupChoice>>,
    #[serde(default)]
    pub completed: Option<bool>,
    #[serde(default)]
    pub skipped: Option<bool>,
}

/// One store row of the setup screen.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetupStoreInfo {
    pub id: SetupStore,
    pub name: &'static str,
    pub sign_in: bool,
    pub note: &'static str,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetupInfo {
    pub state: Option<SetupState>,
    /// Show the full-window setup now (fresh install, setup not finished).
    pub show: bool,
    /// Show the dismissible "Add store accounts" banner (existing library, setup never done).
    pub banner: bool,
    pub stores: Vec<SetupStoreInfo>,
}

/// The stored setup, or `None` when there is none or it is unreadable.
pub fn load(conn: &Connection) -> Result<Option<SetupState>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?",
            [SETUP_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored.and_then(|text| match parse_stored(&text) {
        Ok(state) => Some(state),
        Err(e) => {
            log::warn!("Ignoring the stored first-time setup: {e}");
            None
        }
    }))
}

fn parse_stored(text: &str) -> Result<SetupState> {
    let state: SetupState = serde_json::from_str(text)?;
    if state.version != VERSION {
        bail!("unknown version {}", state.version);
    }
    Ok(state)
}

fn save(conn: &Connection, state: &SetupState) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [SETUP_KEY, &serde_json::to_string(state)?],
    )?;
    Ok(())
}

/// Applies a patch to `current` at `now`. Refuses a sign-in choice for a store this version
/// cannot sign in to, a `false` flag, and finishing and skipping at once.
pub fn apply_patch(current: Option<SetupState>, patch: SetupPatch, now: i64) -> Result<SetupState> {
    let mut next = current.unwrap_or(SetupState {
        version: VERSION,
        ..Default::default()
    });
    if patch.completed == Some(false) || patch.skipped == Some(false) {
        bail!("Setup can only be marked finished or skipped.");
    }
    if patch.completed == Some(true) && patch.skipped == Some(true) {
        bail!("Setup cannot be finished and skipped at once.");
    }
    if let Some(choices) = patch.choices {
        for (store, choice) in &choices {
            if *choice == SetupChoice::Signin && !store.sign_in() {
                bail!(
                    "Signing in to {} is not available in this version.",
                    store.name()
                );
            }
        }
        next.choices.extend(choices);
    }
    if patch.completed == Some(true) {
        next.completed_at = Some(now);
    }
    if patch.skipped == Some(true) {
        next.skipped_at = Some(now);
    }
    Ok(next)
}

/// Whether this library counts as a fresh install: no setup saved, no games, no folders.
pub fn is_fresh(conn: &Connection) -> Result<bool> {
    let setup: Option<i64> = conn
        .query_row("SELECT 1 FROM settings WHERE key=?", [SETUP_KEY], |r| {
            r.get(0)
        })
        .optional()?;
    let roots: i64 = conn.query_row("SELECT COUNT(*) FROM library_roots", [], |r| r.get(0))?;
    Ok(setup.is_none() && roots == 0 && crate::db::game_count(conn)? == 0)
}

fn info(state: Option<SetupState>, fresh: bool) -> SetupInfo {
    let finished = state.as_ref().is_some_and(SetupState::finished);
    SetupInfo {
        show: fresh && !finished,
        banner: !fresh && !finished,
        state,
        stores: SetupStore::ALL
            .iter()
            .map(|&id| SetupStoreInfo {
                id,
                name: id.name(),
                sign_in: id.sign_in(),
                note: id.note(),
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn setup_get(window: WebviewWindow, state: State<'_, AppState>) -> CmdResult<SetupInfo> {
    require_main(&window)?;
    let fresh = state.accounts.fresh_install.load(Ordering::Relaxed);
    Ok(info(load(&state.db.lock()).map_err(err)?, fresh))
}

#[tauri::command]
pub async fn setup_save(
    window: WebviewWindow,
    state: State<'_, AppState>,
    patch: SetupPatch,
) -> CmdResult<SetupInfo> {
    require_main(&window)?;
    let conn = state.db.lock();
    let next = apply_patch(load(&conn).map_err(err)?, patch, now_ms()).map_err(err)?;
    save(&conn, &next).map_err(err)?;
    let fresh = state.accounts.fresh_install.load(Ordering::Relaxed);
    Ok(info(Some(next), fresh))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(json: &str) -> Result<SetupPatch, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn patches_accept_only_known_fields_and_values() {
        assert!(patch(r#"{"choices":{"steam":"signin","gog":"installed","ea":"skip"}}"#).is_ok());
        assert!(patch(r#"{"completed":true}"#).is_ok());
        assert!(patch("{}").is_ok());
        for bad in [
            r#"{"choices":{"amazon":"installed"}}"#,
            r#"{"choices":{"steam":"maybe"}}"#,
            r#"{"choices":{"Steam":"signin"}}"#,
            r#"{"choices":["steam"]}"#,
            r#"{"completedAt":1}"#,
            r#"{"version":2}"#,
            r#"{"completed":"yes"}"#,
            r#"{"choices":{"steam":"signin"},"extra":1}"#,
        ] {
            assert!(patch(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sign_in_is_only_accepted_where_this_version_can_sign_in() {
        let p = patch(r#"{"choices":{"steam":"signin","gog":"installed"}}"#).unwrap();
        let state = apply_patch(None, p, 5).unwrap();
        assert_eq!(state.version, VERSION);
        assert_eq!(state.choices[&SetupStore::Steam], SetupChoice::Signin);
        assert_eq!(state.completed_at, None);
        for store in [
            "gog",
            "epic",
            "xbox",
            "battlenet",
            "humble",
            "ea",
            "ubisoft",
            "itch",
        ] {
            let p = patch(&format!(r#"{{"choices":{{"{store}":"signin"}}}}"#)).unwrap();
            assert!(apply_patch(Some(state.clone()), p, 6).is_err(), "{store}");
        }
        // Later patches merge; the times come from Rust.
        let p = patch(r#"{"choices":{"ea":"skip"},"completed":true}"#).unwrap();
        let done = apply_patch(Some(state), p, 9).unwrap();
        assert_eq!(done.completed_at, Some(9));
        assert_eq!(done.choices.len(), 3);
        assert!(apply_patch(None, patch(r#"{"completed":false}"#).unwrap(), 1).is_err());
        let both = patch(r#"{"completed":true,"skipped":true}"#).unwrap();
        assert!(apply_patch(None, both, 1).is_err());
    }

    #[test]
    fn stored_setup_is_read_strictly() {
        let database = crate::db::Db::memory().unwrap();
        let conn = database.lock();
        assert_eq!(load(&conn).unwrap(), None);
        let state = apply_patch(None, patch(r#"{"skipped":true}"#).unwrap(), 3).unwrap();
        save(&conn, &state).unwrap();
        assert_eq!(load(&conn).unwrap(), Some(state));
        for bad in [
            r#"{"version":2,"choices":{}}"#,
            r#"{"version":1,"choices":{"amazon":"skip"}}"#,
            r#"{"version":1,"choices":{},"token":"x"}"#,
            "not json",
        ] {
            conn.execute("UPDATE settings SET value=? WHERE key='setup'", [bad])
                .unwrap();
            assert_eq!(load(&conn).unwrap(), None, "{bad}");
        }
    }

    #[test]
    fn the_setup_shows_on_a_fresh_install_and_the_banner_otherwise() {
        let database = crate::db::Db::memory().unwrap();
        let conn = database.lock();
        assert!(is_fresh(&conn).unwrap());
        let fresh = info(None, true);
        assert!(fresh.show && !fresh.banner);
        let existing = info(None, false);
        assert!(!existing.show && existing.banner);
        // Choices saved mid-way keep the setup showing; finishing or skipping ends both.
        let p = patch(r#"{"choices":{"steam":"installed"}}"#).unwrap();
        let partial = apply_patch(None, p, 1).unwrap();
        assert!(info(Some(partial.clone()), true).show);
        let done = apply_patch(Some(partial), patch(r#"{"completed":true}"#).unwrap(), 2).unwrap();
        assert!(!info(Some(done.clone()), true).show);
        assert!(!info(Some(done), false).banner);
        // A saved setup row, a game or a folder means not fresh.
        let empty = SetupState {
            version: 1,
            ..Default::default()
        };
        save(&conn, &empty).unwrap();
        assert!(!is_fresh(&conn).unwrap());
        conn.execute("DELETE FROM settings WHERE key='setup'", [])
            .unwrap();
        conn.execute(
            "INSERT INTO library_roots(id,kind,path,enabled,addedAt) VALUES('r','pc','D:/Games',1,0)",
            [],
        )
        .unwrap();
        assert!(!is_fresh(&conn).unwrap());
        // Only Steam offers sign-in; every store has a note.
        let stores = info(None, true).stores;
        assert_eq!(stores.len(), SetupStore::ALL.len());
        assert!(stores
            .iter()
            .all(|s| s.sign_in == (s.id == SetupStore::Steam) && !s.note.is_empty()));
    }
}
