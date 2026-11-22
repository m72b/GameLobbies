//! The store accounts synced from Rust (GOG, Battle.net): which hosts each uses, the probe that
//! decides whether a session works, and syncing (on scans, on "Sync now", after sign-in).
//!
//! GOG signs in in the browser and uses OAuth tokens (GOG Galaxy's public client, see
//! [`super::gog`]); Battle.net signs in in Game Lobby's window and uses the store's cookies.
//! A sync loads the sealed session (refreshing GOG's token when it is about to expire), fetches
//! with [`StoreHttp`] (host allowlist, redirect policy, caps, pacing), applies the result
//! ([`super::apply::apply_account`]) and seals the session again (cookies or tokens may have
//! changed). It runs under the scan lock, like the Steam import and sign-out, so the library is
//! never written twice at once.

use super::apply::{self, FetchOutcome};
use super::http::{FetchError, StoreHttp};
use super::session::{self, Session};
use super::{battlenet, gog, AccountMode, AccountStore, SyncStatus};
use crate::state::AppState;
use crate::types::now_ms;
use anyhow::Result;
use tauri::{AppHandle, Emitter, Manager};
use url::Url;

/// The longest a whole sync may take (it holds the scan lock).
pub const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// The longest a token renewal may take (before the sync's own time limit starts).
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// GOG's playtime requests stop this long before [`SYNC_TIMEOUT`], so the games are applied.
const FETCH_MARGIN: std::time::Duration = std::time::Duration::from_secs(60);
/// GOG's access token is renewed when it has less than this left.
const TOKEN_MARGIN_MS: i64 = 5 * 60 * 1000;

pub const SYNCED_STORES: [AccountStore; 2] = [AccountStore::Gog, AccountStore::Battlenet];

pub fn is_synced_store(store: AccountStore) -> bool {
    SYNCED_STORES.contains(&store)
}

/// The hosts requests may go to (exact names).
pub fn api_hosts(store: AccountStore) -> &'static [&'static str] {
    match store {
        AccountStore::Gog => gog::API_HOSTS,
        AccountStore::Battlenet => battlenet::API_HOSTS,
        _ => &[],
    }
}

/// The URLs whose cookies are read from the sign-in window (Battle.net): every API host's root
/// and the store's own request URLs (a cookie set for a path is only sent there).
pub fn cookie_urls(store: AccountStore) -> Vec<String> {
    let requests: &[&str] = match store {
        AccountStore::Battlenet => &[
            "https://account.battle.net/api/games-and-subs",
            "https://account.battle.net/api/classic-games",
        ],
        _ => return Vec::new(),
    };
    let mut urls: Vec<String> = api_hosts(store)
        .iter()
        .map(|host| format!("https://{host}/"))
        .collect();
    urls.extend(requests.iter().map(|u| u.to_string()));
    urls
}

/// The domains (and their subdomains) whose cookies are kept.
pub fn cookie_domains(store: AccountStore) -> &'static [&'static str] {
    match store {
        AccountStore::Battlenet => battlenet::COOKIE_DOMAINS,
        _ => &[],
    }
}

/// The first page of the store's sign-in window (Battle.net).
pub fn start_url(store: AccountStore) -> Option<Url> {
    match store {
        AccountStore::Battlenet => Url::parse(battlenet::START_URL).ok(),
        _ => None,
    }
}

/// Whether the sign-in window shows the store's signed-in page (time to take the cookies).
pub fn signed_in_page(store: AccountStore, url: &Url) -> bool {
    match store {
        AccountStore::Battlenet => battlenet::signed_in_page(url),
        _ => false,
    }
}

/// Checks a window session; the account name when the store gives one.
pub async fn probe(store: AccountStore, http: &StoreHttp) -> Result<Option<String>, FetchError> {
    match store {
        AccountStore::Battlenet => battlenet::probe(http).await,
        _ => Err(FetchError::Invalid("this store has no sign-in window.")),
    }
}

/// The client for a session, with GOG's tokens renewed first when they are about to expire
/// (then `Some(new session)` to seal).
async fn client_for(
    store: AccountStore,
    saved: &Session,
) -> Result<(StoreHttp, Option<Session>), FetchError> {
    match saved {
        Session::Cookies { .. } => {
            let (jar, user_agent) = saved.jar(cookie_domains(store));
            Ok((StoreHttp::new(api_hosts(store), jar, &user_agent)?, None))
        }
        Session::Tokens {
            access,
            refresh,
            expires_at,
            user_id,
            account,
            ..
        } => {
            if *expires_at - now_ms() > TOKEN_MARGIN_MS {
                let http = StoreHttp::new(api_hosts(store), Default::default(), "")?;
                return Ok((http.with_bearer(access), None));
            }
            let auth = StoreHttp::new(gog::TOKEN_HOSTS, Default::default(), "")?;
            let mut tokens = gog::refresh(&auth, &gog::Endpoints::production(), refresh).await?;
            if tokens.user_id != *user_id {
                // A refresh never changes the account; refuse an answer that does.
                return Err(FetchError::Invalid(
                    "the renewed sign-in is for another account.",
                ));
            }
            tokens.user_id = user_id.clone();
            let renewed = Session::tokens(&tokens, account.clone(), now_ms());
            let http = StoreHttp::new(api_hosts(store), Default::default(), "")?;
            Ok((http.with_bearer(&tokens.access), Some(renewed)))
        }
    }
}

async fn fetch(
    store: AccountStore,
    http: &StoreHttp,
    saved: &Session,
    playtime_ages: &std::collections::HashMap<String, i64>,
) -> Result<FetchOutcome, FetchError> {
    match (store, saved) {
        (AccountStore::Gog, Session::Tokens { user_id, .. }) => {
            let deadline = std::time::Instant::now() + (SYNC_TIMEOUT - FETCH_MARGIN);
            gog::fetch(
                http,
                &gog::Endpoints::production(),
                user_id,
                playtime_ages,
                deadline,
            )
            .await
        }
        (AccountStore::Battlenet, Session::Cookies { .. }) => battlenet::fetch(http).await,
        _ => Err(FetchError::Rejected),
    }
}

/// Syncs `store` when it is signed in, set to Account and (unless `force`) due. The caller holds
/// the scan lock. `None` when nothing was requested.
pub async fn sync_store(
    app: &AppHandle,
    store: AccountStore,
    force: bool,
) -> Result<Option<SyncStatus>> {
    let state = app.state::<AppState>();
    let loaded = {
        let conn = state.db.lock();
        let account = super::load_account(&conn, store)?;
        let paused = account.mode == Some(AccountMode::Installed);
        let due = force || apply::is_due(&account.sync.clone().unwrap_or_default(), now_ms());
        if paused || !due {
            return Ok(None);
        }
        session::load(&conn, store)?
    };
    let Some(saved) = loaded else {
        return Ok(None);
    };
    // Renewing the token comes first, with its own time limit, and a renewed session is sealed
    // at once: if GOG replaces the refresh token, the new one is never lost to a slow fetch.
    let renewal = tokio::time::timeout(REFRESH_TIMEOUT, client_for(store, &saved))
        .await
        .unwrap_or(Err(FetchError::Network(
            "the store took too long to answer.",
        )));
    let (client, saved) = match renewal {
        Ok((client, Some(renewed))) => {
            let conn = state.db.lock();
            if session::exists(&conn, store)? {
                session::save(&conn, store, &renewed)?;
            }
            (client, renewed)
        }
        Ok((client, None)) => (client, saved),
        Err(e) => {
            let status = {
                let mut conn = state.db.lock();
                apply::apply_account(&mut conn, store, &Err(e), now_ms(), None)?
            };
            log::warn!("{} account: {}", store.name(), status.message);
            return Ok(Some(status));
        }
    };
    let ages = if store == AccountStore::Gog {
        crate::stats::store_playtime_ages(&state.db.lock(), crate::stats::GOG)?
    } else {
        Default::default()
    };
    let outcome = tokio::time::timeout(SYNC_TIMEOUT, fetch(store, &client, &saved, &ages))
        .await
        .unwrap_or(Err(FetchError::Network(
            "the store took too long to answer.",
        )));
    let (http, renewed): (Option<StoreHttp>, Option<Session>) = (Some(client), None);
    let status = {
        let mut conn = state.db.lock();
        let retry_after = http.as_ref().and_then(StoreHttp::retry_after);
        let status = apply::apply_account(&mut conn, store, &outcome, now_ms(), retry_after)?;
        // Seal what changed (only while still signed in): renewed tokens, refreshed cookies.
        if session::exists(&conn, store)? {
            let next = match (&saved, renewed, &http) {
                (_, Some(renewed), _) => Some(renewed),
                (Session::Cookies { account, .. }, None, Some(http)) => Some(Session::cookies(
                    &http.cookies(),
                    http.user_agent(),
                    account.clone(),
                    now_ms(),
                )),
                _ => None,
            };
            if let Some(next) = next {
                session::save(&conn, store, &next)?;
            }
        }
        status
    };
    if status.ok {
        log::info!("{} account: {}", store.name(), status.message);
        state.tracker.library_changed();
        let _ = app.emit("library:changed", ());
        crate::covercache::spawn_pass(app, std::time::Duration::ZERO);
    } else {
        log::warn!("{} account: {}", store.name(), status.message);
    }
    Ok(Some(status))
}

/// Called by the library scan (the scan lock is held). Error lines for the scan summary.
pub async fn run_for_scan(app: &AppHandle) -> Vec<String> {
    let mut errors = Vec::new();
    for store in SYNCED_STORES {
        if store_disabled(app, store) {
            continue;
        }
        match sync_store(app, store, false).await {
            Ok(Some(status)) if !status.ok => {
                errors.push(format!("{} account: {}", store.name(), status.message))
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("{} account: {e:#}", store.name())),
        }
    }
    super::notify(app);
    errors
}

/// Whether the store is turned off under Libraries (then its account isn't synced either).
fn store_disabled(app: &AppHandle, store: AccountStore) -> bool {
    let state = app.state::<AppState>();
    let conn = state.db.lock();
    crate::db::get_settings(&conn)
        .map(|s| s.disabled_stores.iter().any(|d| d == store.id()))
        .unwrap_or(false)
}

/// Forgets the account (under the scan lock; a sign-in that started before is discarded).
pub async fn sign_out(app: &AppHandle, store: AccountStore) -> Result<usize> {
    let state = app.state::<AppState>();
    let _guard = state.scan_lock.lock().await;
    state.accounts.bump_generation(store);
    let deleted = apply::sign_out(&mut state.db.lock(), store)?;
    log::info!(
        "{} account removed ({deleted} games removed from the library)",
        store.name()
    );
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
    Ok(deleted)
}

/// Installed files only: pauses syncing and removes what the account added.
pub async fn set_installed(app: &AppHandle, store: AccountStore) -> Result<usize> {
    let state = app.state::<AppState>();
    let _guard = state.scan_lock.lock().await;
    let deleted = {
        let mut conn = state.db.lock();
        // The pause first: a failed removal can't be undone by a sync.
        super::save_mode(&conn, store, AccountMode::Installed)?;
        apply::undo_account(&mut conn, store)?
    };
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
    Ok(deleted)
}

/// Account mode again: lifts the pause and syncs.
pub async fn set_account(app: &AppHandle, store: AccountStore) -> Result<Option<SyncStatus>> {
    let state = app.state::<AppState>();
    let _guard = state.scan_lock.lock().await;
    super::clear_mode(&state.db.lock(), store)?;
    sync_store(app, store, true).await
}

pub async fn sync_now(app: &AppHandle, store: AccountStore) -> Result<Option<SyncStatus>> {
    let state = app.state::<AppState>();
    let _guard = state.scan_lock.lock().await;
    sync_store(app, store, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_read_for_the_roots_and_the_request_urls_of_listed_hosts_only() {
        let urls = cookie_urls(AccountStore::Battlenet);
        assert!(urls.len() > api_hosts(AccountStore::Battlenet).len());
        for url in urls {
            let url = Url::parse(&url).unwrap();
            assert_eq!(url.scheme(), "https");
            assert!(api_hosts(AccountStore::Battlenet).contains(&url.host_str().unwrap()));
        }
        // GOG uses tokens, not cookies; Steam neither.
        assert!(cookie_urls(AccountStore::Gog).is_empty());
        assert!(cookie_domains(AccountStore::Gog).is_empty());
        assert!(cookie_urls(AccountStore::Steam).is_empty());
    }

    #[test]
    fn a_sync_is_bounded_in_time_and_renews_tokens_first() {
        let source = include_str!("stores.rs");
        let sync = &source[source.find("pub async fn sync_store").unwrap()..];
        // The renewal has its own limit and is sealed before the fetch's limit starts.
        let renew = sync
            .find("tokio::time::timeout(REFRESH_TIMEOUT, client_for(store, &saved))")
            .unwrap();
        let seal = sync.find("session::save(&conn, store, &renewed)?").unwrap();
        let fetch = sync
            .find("tokio::time::timeout(SYNC_TIMEOUT, fetch(store, &client, &saved, &ages))")
            .unwrap();
        assert!(renew < seal && seal < fetch);
        assert!(FETCH_MARGIN < SYNC_TIMEOUT);
        assert!(SYNC_TIMEOUT <= std::time::Duration::from_secs(300));
        // The bearer token only goes to GOG's API hosts; the token endpoint gets none.
        let client = &source[source.find("async fn client_for").unwrap()..];
        let client = &client[..client.find("\nasync fn fetch").unwrap()];
        assert!(client.contains("StoreHttp::new(gog::TOKEN_HOSTS, Default::default(), \"\")?"));
        assert_eq!(client.matches(".with_bearer(").count(), 2);
        assert!(client.matches("StoreHttp::new(api_hosts(store)").count() >= 2);
    }

    #[test]
    fn the_battlenet_window_starts_on_an_allowed_page() {
        let store = AccountStore::Battlenet;
        let start = start_url(store).unwrap();
        assert!(super::super::signin::navigation_allowed(
            &start,
            super::super::signin::login_hosts(store)
        ));
        for host in api_hosts(store) {
            assert!(crate::accounts::cookies::domain_allowed(
                host,
                cookie_domains(store)
            ));
        }
        // Battle.net starts on the signed-in page itself (it sends a signed-out user to the
        // sign-in).
        assert!(signed_in_page(store, &start));
        assert!(start_url(AccountStore::Gog).is_none());
        assert!(start_url(AccountStore::Epic).is_none());
        assert!(is_synced_store(AccountStore::Gog) && !is_synced_store(AccountStore::Steam));
    }
}
