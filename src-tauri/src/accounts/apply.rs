//! Shared rules for applying a store's list of owned games to the library: the mass-removal
//! guard (also used by the Steam owned-games import) and the apply step of the cookie-based
//! store accounts (GOG, Battle.net).
//!
//! Account games are added only when the library has nothing for them yet: a game with the same
//! id, a route of the same store with the same id (an installed game the store's own scan found),
//! or a game of the same store with the same title is left exactly as it is. New entries are
//! owned, not installed, with an `<store>-account` route; titles, covers, favourites, notes and
//! routes of existing games are never changed. Only games this import created can be taken out
//! again, only after a complete list, and behind the mass-removal guard.

use super::http::FetchError;
use super::{AccountStore, SyncStatus};
use crate::db;
use crate::types::{Game, GameRoute};
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Smallest earlier list the mass-removal guard applies to.
pub const MASS_REMOVAL_MIN: usize = 20;
/// How long a held-back removal must stand before a later fetch may apply it.
pub const PENDING_MIN_AGE_MS: i64 = 6 * 60 * 60 * 1000;

/// What a fetch may take out of the library now, and what it holds back.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemovalPlan {
    pub to_drop: HashSet<String>,
    pub held: HashSet<String>,
    /// When the held set was first held back (0 = nothing held).
    pub pending_since: i64,
}

/// The mass-removal guard. `previous`: ids the import added before; `gone`: those of them the
/// new list no longer has; `was_pending` / `pending_since`: what an earlier fetch held back.
///
/// A list that drops more than half of an earlier list of [`MASS_REMOVAL_MIN`] or more is more
/// likely degraded than true: nothing is removed until a later fetch, at least
/// [`PENDING_MIN_AGE_MS`] after the first, reports (nearly, 90 %) the same ids gone. The clock
/// starts when a removal is first held; a different held set restarts it.
pub fn plan_removal(
    previous: &HashSet<String>,
    gone: &HashSet<String>,
    was_pending: &HashSet<String>,
    pending_since: i64,
    now: i64,
) -> RemovalPlan {
    let suspicious = previous.len() >= MASS_REMOVAL_MIN && gone.len() * 2 > previous.len();
    let (to_drop, held): (HashSet<String>, HashSet<String>) = if suspicious {
        let agreed: HashSet<String> = gone.intersection(was_pending).cloned().collect();
        let old_enough = !was_pending.is_empty()
            && pending_since > 0
            && now >= pending_since
            && now - pending_since >= PENDING_MIN_AGE_MS;
        if old_enough && agreed.len() * 10 >= was_pending.len() * 9 {
            let held = gone.difference(&agreed).cloned().collect();
            (agreed, held)
        } else {
            (HashSet::new(), gone.clone())
        }
    } else {
        (gone.clone(), HashSet::new())
    };
    let pending_since = if held.is_empty() {
        0
    } else if !was_pending.is_empty() && pending_since > 0 && held == *was_pending {
        pending_since
    } else {
        now
    };
    RemovalPlan {
        to_drop,
        held,
        pending_since,
    }
}

// ---------------------------------------------------------------------------
// Store accounts synced from Rust (GOG, Battle.net)

/// After a good sync, scans don't ask the store again for this long.
pub const REFRESH_AFTER_MS: i64 = 6 * 60 * 60 * 1000;
/// After a failed sync, scans wait this long.
pub const RETRY_AFTER_MS: i64 = 30 * 60 * 1000;
/// The longest wait after the store asked to slow down.
pub const MAX_BACKOFF_MS: i64 = 60 * 60 * 1000;

/// One owned game from a store.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedGame {
    /// The store's id, already validated (GOG product id, Battle.net code).
    pub store_id: String,
    pub title: String,
    /// A validated store link, or an `owned-only:` target.
    pub launch: Option<String>,
    pub playtime_seconds: Option<i64>,
    pub last_played_ms: Option<i64>,
}

/// One fetch of a store's library.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FetchOutcome {
    pub games: Vec<OwnedGame>,
    /// Every owned id, titled or not: what is gone is decided against this.
    pub owned_ids: HashSet<String>,
    /// Every page was read: only then may anything be removed.
    pub complete: bool,
    /// Every playtime request was answered: only then is the store's playtime replaced.
    pub playtime_complete: bool,
}

/// What the account import did so far (kept in the `account:<store>` row).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncState {
    /// Store ids this import added (what it may take out again).
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub pending_gone: Vec<String>,
    #[serde(default)]
    pub pending_since: i64,
    #[serde(default)]
    pub attempted_at: i64,
    /// No sync before this time (backoff after errors and rate limits).
    #[serde(default)]
    pub next_at: i64,
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub status: Option<SyncStatus>,
}

/// Whether a scan should sync the store now.
pub fn is_due(sync: &SyncState, now: i64) -> bool {
    if sync.attempted_at <= 0 || sync.attempted_at > now {
        return true;
    }
    if sync.status.as_ref().is_some_and(|s| s.needs_sign_in) {
        return false;
    }
    now >= sync.next_at
}

/// The library id of a store's game: `gog:<id>`, `battlenet:<code>`.
pub fn game_id(store: AccountStore, store_id: &str) -> String {
    format!("{}:{store_id}", store.id())
}

/// The route source of games an account added: `<store>-account`.
pub fn route_source(store: AccountStore) -> String {
    format!("{}-account", store.id())
}

fn account_game(store: AccountStore, game: &OwnedGame) -> Game {
    crate::importers::single_route_game(
        game_id(store, &game.store_id),
        game.title.clone(),
        store.name(),
        None,
        GameRoute {
            source_id: route_source(store),
            store: store.name().into(),
            app_id: Some(game.store_id.clone()),
            launch_target: game.launch.clone().unwrap_or_else(|| {
                format!(
                    "{}{}:{}",
                    crate::launch::OWNED_ONLY_PREFIX,
                    store.id(),
                    game.store_id
                )
            }),
            owned: Some(true),
            installed: Some(false),
            ..Default::default()
        },
    )
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// Applies one sync of `store` made at `now`. Success: new owned games are added (nothing that
/// is already in the library is touched), GOG playtime replaced, and, when the list is
/// complete, games this import added that are gone are removed behind the mass-removal guard.
/// Failure: the library stays as it is; the status says why and when to retry
/// (`retry_after_s`: what a 429 asked for). Either way the state is stored.
pub fn apply_account(
    conn: &mut Connection,
    store: AccountStore,
    outcome: &Result<FetchOutcome, FetchError>,
    now: i64,
    retry_after_s: Option<u64>,
) -> Result<SyncStatus> {
    let mut account = super::load_account(conn, store)?;
    let mut sync = account.sync.take().unwrap_or_default();
    sync.attempted_at = now;
    let name = store.name();
    // A complete list with no games at all after a non-empty one is far more likely a changed
    // page than an emptied account: it is treated as unreadable, so nothing is ever removed.
    let empty = Err(FetchError::Invalid("the store returned no games."));
    let outcome = match outcome {
        Ok(list) if list.complete && list.owned_ids.is_empty() && !sync.ids.is_empty() => &empty,
        other => other,
    };
    let status = match outcome {
        Ok(list) => {
            let source = route_source(store);
            let previous: HashSet<String> = sync.ids.iter().cloned().collect();
            let games = db::all_games(conn)?;
            let ids_present: HashSet<&str> = games.iter().map(|g| g.id.as_str()).collect();
            let same_store_ids: HashSet<&str> = games
                .iter()
                .flat_map(|g| g.routes.iter())
                .filter(|r| r.store == name && r.source_id != source)
                .filter_map(|r| r.app_id.as_deref())
                .collect();
            let same_store_titles: HashSet<String> = games
                .iter()
                .filter(|g| g.store == name)
                .filter(|g| !g.routes.iter().all(|r| r.source_id == source))
                .map(|g| crate::catalog::title_key(&g.title))
                .collect();
            let mut created: BTreeSet<String> = BTreeSet::new();
            let mut new_games = Vec::new();
            for game in &list.games {
                let id = game_id(store, &game.store_id);
                let ours = previous.contains(&game.store_id);
                if ids_present.contains(id.as_str())
                    || same_store_ids.contains(game.store_id.as_str())
                    || (!ours
                        && same_store_titles.contains(&crate::catalog::title_key(&game.title)))
                    || !created.insert(game.store_id.clone())
                {
                    continue;
                }
                new_games.push(account_game(store, game));
            }
            db::upsert_imported_games(conn, &new_games)?;
            if let (Some(playtime_source), true) = (
                playtime_source(store),
                list.complete && list.playtime_complete,
            ) {
                let horizon = now.saturating_add(86_400_000);
                let seconds: HashMap<String, i64> = list
                    .games
                    .iter()
                    .filter_map(|g| Some((g.store_id.clone(), g.playtime_seconds?)))
                    .collect();
                let last: HashMap<String, i64> = list
                    .games
                    .iter()
                    .filter_map(|g| Some((g.store_id.clone(), g.last_played_ms?)))
                    .filter(|(_, ms)| *ms <= horizon)
                    .collect();
                crate::stats::replace_store_playtime(conn, playtime_source, &seconds, &last, now)?;
            }
            let gone: HashSet<String> = if list.complete {
                previous
                    .iter()
                    .filter(|id| !list.owned_ids.contains(*id))
                    .cloned()
                    .collect()
            } else {
                HashSet::new()
            };
            let was_pending: HashSet<String> = sync.pending_gone.iter().cloned().collect();
            let plan = if list.complete {
                plan_removal(&previous, &gone, &was_pending, sync.pending_since, now)
            } else {
                RemovalPlan {
                    held: was_pending.clone(),
                    pending_since: sync.pending_since,
                    ..Default::default()
                }
            };
            db::drop_owned(conn, &source, &plan.to_drop)?;
            let mut ids: BTreeSet<String> = previous
                .iter()
                .filter(|id| !plan.to_drop.contains(*id))
                .cloned()
                .collect();
            ids.extend(created.iter().cloned());
            sync.ids = ids.into_iter().collect();
            sync.pending_since = plan.pending_since;
            let mut pending: Vec<String> = plan.held.iter().cloned().collect();
            pending.sort();
            sync.pending_gone = pending;
            sync.failures = 0;
            sync.next_at = now + REFRESH_AFTER_MS;
            let message = if !plan.held.is_empty() && plan.to_drop.is_empty() && list.complete {
                format!("{name} returned far fewer games than before; nothing was removed. It will be checked again later.")
            } else {
                format!(
                    "{} on {name} · {} added{}",
                    plural(list.owned_ids.len(), "owned game"),
                    created.len(),
                    if list.complete {
                        ""
                    } else {
                        " · the list was incomplete, nothing was removed"
                    }
                )
            };
            SyncStatus {
                ok: true,
                message,
                checked_at: now,
                needs_sign_in: false,
            }
        }
        Err(e) => {
            sync.failures = sync.failures.saturating_add(1);
            let wait = match e {
                FetchError::RateLimited => retry_after_s
                    .map(|s| (s as i64).saturating_mul(1000))
                    .unwrap_or_else(|| RETRY_AFTER_MS.saturating_mul(1 << sync.failures.min(4)))
                    .min(MAX_BACKOFF_MS),
                _ => RETRY_AFTER_MS,
            };
            sync.next_at = now + wait;
            let needs_sign_in = *e == FetchError::Rejected;
            SyncStatus {
                ok: false,
                message: if needs_sign_in {
                    format!("{name} did not accept the saved sign-in (it may have expired). Sign in again.")
                } else {
                    e.message_for(name)
                },
                checked_at: now,
                needs_sign_in,
            }
        }
    };
    sync.status = Some(status.clone());
    account.sync = Some(sync);
    super::save_account(conn, store, &account)?;
    Ok(status)
}

/// The playtime source of a store's account, if it reports playtime.
fn playtime_source(store: AccountStore) -> Option<&'static str> {
    (store == AccountStore::Gog).then_some(crate::stats::GOG)
}

/// Removes what the account import added (games the user put nothing into), its playtime and
/// its sync state; the mode stays. The number of games deleted.
pub fn undo_account(conn: &mut Connection, store: AccountStore) -> Result<usize> {
    let mut account = super::load_account(conn, store)?;
    let ids: HashSet<String> = account
        .sync
        .take()
        .map(|s| s.ids.into_iter().collect())
        .unwrap_or_default();
    let deleted = db::drop_owned(conn, &route_source(store), &ids)?;
    if let Some(source) = playtime_source(store) {
        crate::stats::clear_store_playtime(conn, source)?;
    }
    super::save_account(conn, store, &account)?;
    Ok(deleted)
}

/// Sign-out: [`undo_account`], then the sealed session and the `account:<store>` row go.
pub fn sign_out(conn: &mut Connection, store: AccountStore) -> Result<usize> {
    let deleted = undo_account(conn, store)?;
    super::session::delete(conn, store)?;
    super::forget_mode(conn, store)?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(id: &str, title: &str, minutes: Option<i64>) -> OwnedGame {
        OwnedGame {
            store_id: id.into(),
            title: title.into(),
            launch: Some(format!("goggalaxy://openGameView/{id}")),
            playtime_seconds: minutes.map(|m| m * 60),
            last_played_ms: minutes.map(|_| 1_700_000_000_000),
        }
    }

    fn outcome(games: Vec<OwnedGame>, complete: bool) -> Result<FetchOutcome, FetchError> {
        Ok(FetchOutcome {
            owned_ids: games.iter().map(|g| g.store_id.clone()).collect(),
            games,
            complete,
            playtime_complete: complete,
        })
    }

    fn galaxy_game(id: &str, title: &str) -> Game {
        crate::importers::single_route_game(
            format!("gog:{id}"),
            title.into(),
            "GOG",
            None,
            GameRoute {
                source_id: "gog-galaxy".into(),
                store: "GOG".into(),
                app_id: Some(id.into()),
                launch_target: format!("goggalaxy://openGameView/{id}"),
                owned: Some(true),
                installed: Some(true),
                ..Default::default()
            },
        )
    }

    #[test]
    fn account_games_never_touch_what_the_library_already_has() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        // Installed by Galaxy (same id), renamed by the user; another GOG game with a title the
        // account also lists under a different id.
        db::upsert_imported_games(
            &mut conn,
            &[galaxy_game("1", "Witcher"), galaxy_game("50", "Gwent")],
        )
        .unwrap();
        conn.execute(
            "UPDATE games SET title='My Witcher',favorite=1 WHERE id='gog:1'",
            [],
        )
        .unwrap();
        let before = db::get_game(&conn, "gog:1").unwrap().unwrap();
        let status = apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(
                vec![
                    owned("1", "The Witcher", Some(90)),
                    owned("2", "Gwent", None),
                    owned("3", "Cyberpunk", Some(5)),
                ],
                true,
            ),
            1000,
            None,
        )
        .unwrap();
        assert!(status.ok, "{}", status.message);
        assert_eq!(status.message, "3 owned games on GOG · 1 added");
        assert_eq!(db::get_game(&conn, "gog:1").unwrap().unwrap(), before);
        assert!(
            db::get_game(&conn, "gog:2").unwrap().is_none(),
            "same title, same store: skipped"
        );
        let new = db::get_game(&conn, "gog:3").unwrap().unwrap();
        assert_eq!(new.store, "GOG");
        let route = &new.routes[0];
        assert_eq!(
            (
                route.source_id.as_str(),
                route.owned,
                route.installed,
                route.launch_target.as_str()
            ),
            (
                "gog-account",
                Some(true),
                Some(false),
                "goggalaxy://openGameView/3"
            )
        );
        assert_eq!(
            new.playtime_seconds,
            Some(0),
            "tracked playtime is never written"
        );
        // GOG's playtime is kept apart and counts for the Galaxy game too.
        let times = crate::stats::store_times(&conn).unwrap();
        assert_eq!(times["gog:1"].seconds, 5400);
        assert_eq!(times["gog:3"].source, "gog");
        let account = crate::accounts::load_account(&conn, AccountStore::Gog).unwrap();
        assert_eq!(account.sync.unwrap().ids, ["3"]);
    }

    #[test]
    fn incomplete_lists_remove_nothing_and_gone_games_go_behind_the_guard() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        let many: Vec<OwnedGame> = (1..=30)
            .map(|i| owned(&i.to_string(), &format!("Game {i}"), None))
            .collect();
        apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(many.clone(), true),
            1000,
            None,
        )
        .unwrap();
        assert_eq!(db::game_count(&conn).unwrap(), 30);
        // The user played one of them: it stays even when it leaves the account.
        conn.execute("UPDATE games SET favorite=1 WHERE id='gog:2'", [])
            .unwrap();
        // An incomplete list never removes.
        apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(many[..1].to_vec(), false),
            2000,
            None,
        )
        .unwrap();
        assert_eq!(db::game_count(&conn).unwrap(), 30);
        // A complete but far smaller list is held back...
        let status = apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(many[..1].to_vec(), true),
            3000,
            None,
        )
        .unwrap();
        assert!(
            status.message.contains("nothing was removed"),
            "{}",
            status.message
        );
        assert_eq!(db::game_count(&conn).unwrap(), 30);
        // ...until an agreeing list six hours later.
        apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(many[..1].to_vec(), true),
            3000 + PENDING_MIN_AGE_MS,
            None,
        )
        .unwrap();
        assert_eq!(db::game_count(&conn).unwrap(), 2);
        assert!(db::get_game(&conn, "gog:2").unwrap().unwrap().favorite);
        // A small removal applies at once (a list that still has a game: an empty one is
        // not believed after a full one).
        apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(vec![owned("99", "Newer", None)], true),
            4000 + PENDING_MIN_AGE_MS,
            None,
        )
        .unwrap();
        assert!(db::get_game(&conn, "gog:1").unwrap().is_none());
    }

    #[test]
    fn an_empty_list_after_a_full_one_removes_nothing() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        let many: Vec<OwnedGame> = (1..=30)
            .map(|i| owned(&i.to_string(), &format!("Game {i}"), None))
            .collect();
        apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &outcome(many, true),
            1000,
            None,
        )
        .unwrap();
        for at in [
            2000,
            2000 + PENDING_MIN_AGE_MS,
            3000 + 2 * PENDING_MIN_AGE_MS,
        ] {
            let status = apply_account(
                &mut conn,
                AccountStore::Battlenet,
                &outcome(Vec::new(), true),
                at,
                None,
            )
            .unwrap();
            assert!(!status.ok);
            assert!(
                status.message.contains("returned no games"),
                "{}",
                status.message
            );
        }
        assert_eq!(db::game_count(&conn).unwrap(), 30);
        // A first sync of an empty account is fine.
        let status = apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(Vec::new(), true),
            1000,
            None,
        )
        .unwrap();
        assert!(status.ok);
    }

    #[test]
    fn failures_keep_the_library_and_schedule_the_retry() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &outcome(vec![owned("WoW", "World of Warcraft", None)], true),
            1000,
            None,
        )
        .unwrap();
        let sync = |conn: &Connection| {
            crate::accounts::load_account(conn, AccountStore::Battlenet)
                .unwrap()
                .sync
                .unwrap()
        };
        assert_eq!(sync(&conn).next_at, 1000 + REFRESH_AFTER_MS);
        assert!(!is_due(&sync(&conn), 1000 + REFRESH_AFTER_MS - 1));
        assert!(is_due(&sync(&conn), 1000 + REFRESH_AFTER_MS));
        let status = apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &Err(FetchError::RateLimited),
            2000,
            Some(120),
        )
        .unwrap();
        assert!(!status.ok);
        assert_eq!(sync(&conn).next_at, 2000 + 120_000);
        apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &Err(FetchError::RateLimited),
            3000,
            Some(999_999),
        )
        .unwrap();
        assert_eq!(sync(&conn).next_at, 3000 + MAX_BACKOFF_MS);
        apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &Err(FetchError::Status(500)),
            4000,
            None,
        )
        .unwrap();
        assert_eq!(sync(&conn).next_at, 4000 + RETRY_AFTER_MS);
        let status = apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &Err(FetchError::Rejected),
            5000,
            None,
        )
        .unwrap();
        assert!(status.needs_sign_in && status.message.contains("Sign in again"));
        assert!(!is_due(&sync(&conn), i64::MAX / 2));
        assert_eq!(db::game_count(&conn).unwrap(), 1);
        assert_eq!(sync(&conn).ids, ["WoW"]);
    }

    #[test]
    fn sign_out_removes_the_session_games_playtime_and_state() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        let session =
            crate::accounts::session::Session::cookies(&Default::default(), "ua", None, 1);
        crate::accounts::session::save(&conn, AccountStore::Gog, &session).unwrap();
        apply_account(
            &mut conn,
            AccountStore::Gog,
            &outcome(
                vec![owned("7", "Kept", Some(10)), owned("8", "Gone", Some(5))],
                true,
            ),
            1000,
            None,
        )
        .unwrap();
        conn.execute("UPDATE games SET favorite=1 WHERE id='gog:7'", [])
            .unwrap();
        // Another store's data is untouched.
        apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &outcome(vec![owned("cool_game", "Cool", None)], true),
            1000,
            None,
        )
        .unwrap();
        assert_eq!(sign_out(&mut conn, AccountStore::Gog).unwrap(), 1);
        assert!(db::get_game(&conn, "gog:8").unwrap().is_none());
        let kept = db::get_game(&conn, "gog:7").unwrap().unwrap();
        assert_eq!(kept.routes[0].owned, None);
        assert!(!crate::accounts::session::exists(&conn, AccountStore::Gog).unwrap());
        assert_eq!(
            crate::accounts::load_account(&conn, AccountStore::Gog).unwrap(),
            Default::default()
        );
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM store_playtime WHERE source='gog'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
        assert!(db::get_game(&conn, "battlenet:cool_game")
            .unwrap()
            .is_some());
        // Installed files only: the same removal, but the session and the mode stay.
        crate::accounts::session::save(&conn, AccountStore::Battlenet, &session).unwrap();
        assert_eq!(undo_account(&mut conn, AccountStore::Battlenet).unwrap(), 1);
        assert!(crate::accounts::session::exists(&conn, AccountStore::Battlenet).unwrap());
    }

    #[test]
    fn owned_only_entries_are_listed_but_never_launch() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        let game = OwnedGame {
            store_id: "cool_game".into(),
            title: "Cool Game".into(),
            launch: None,
            playtime_seconds: None,
            last_played_ms: None,
        };
        apply_account(
            &mut conn,
            AccountStore::Battlenet,
            &outcome(vec![game], true),
            1000,
            None,
        )
        .unwrap();
        let entry = db::get_game(&conn, "battlenet:cool_game").unwrap().unwrap();
        assert_eq!(entry.store, "Battle.net");
        assert_eq!(
            entry.routes[0].launch_target,
            "owned-only:battlenet:cool_game"
        );
        assert!(crate::launch::is_owned_only(&entry.routes[0].launch_target));
        // The library (and backups) accept it as a route.
        let value = serde_json::to_value(&entry).unwrap();
        assert!(crate::validation::game(&value).is_ok());
    }

    fn ids(range: std::ops::RangeInclusive<usize>) -> HashSet<String> {
        range.map(|i| i.to_string()).collect()
    }

    #[test]
    fn small_or_partial_removals_apply_at_once() {
        let plan = plan_removal(&ids(1..=10), &ids(1..=9), &HashSet::new(), 0, 5);
        assert_eq!(plan.to_drop.len(), 9);
        let plan = plan_removal(&ids(1..=100), &ids(1..=40), &HashSet::new(), 0, 5);
        assert_eq!(
            (plan.to_drop.len(), plan.held.len(), plan.pending_since),
            (40, 0, 0)
        );
    }

    #[test]
    fn a_mass_removal_waits_for_an_agreeing_fetch_six_hours_later() {
        let previous = ids(1..=100);
        let gone = ids(2..=100);
        let first = plan_removal(&previous, &gone, &HashSet::new(), 0, 1000);
        assert!(first.to_drop.is_empty());
        assert_eq!((first.held.len(), first.pending_since), (99, 1000));
        let early = plan_removal(
            &previous,
            &gone,
            &first.held,
            1000,
            1000 + PENDING_MIN_AGE_MS - 1,
        );
        assert!(early.to_drop.is_empty());
        assert_eq!(early.pending_since, 1000);
        let later = plan_removal(
            &previous,
            &gone,
            &first.held,
            1000,
            1000 + PENDING_MIN_AGE_MS,
        );
        assert_eq!(
            (later.to_drop.len(), later.held.len(), later.pending_since),
            (99, 0, 0)
        );
        // A different held set restarts the clock.
        let other = plan_removal(&previous, &ids(50..=100), &first.held, 1000, 9_000_000_000);
        assert!(other.to_drop.is_empty());
        assert_eq!(other.pending_since, 9_000_000_000);
    }
}
