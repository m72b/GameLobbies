//! The cookie-based store accounts (GOG, Battle.net, Humble): which hosts each uses, the probe
//! that decides whether a session works, and syncing (on scans, on "Sync now", after sign-in).
//!
//! A sync loads the sealed session, fetches with [`StoreHttp`] (host allowlist, redirect policy,
//! caps, pacing), applies the result ([`super::apply::apply_account`]) and seals the cookies
//! again (the store may have refreshed them). It runs under the scan lock, like the Steam import
//! and sign-out, so the library is never written twice at once.

use super::apply::{self, FetchOutcome};
use super::http::{FetchError, StoreHttp};
use super::session::{self, Session};
use super::{battlenet, gog, humble, AccountMode, AccountStore, SyncStatus};
use crate::state::AppState;
use crate::types::now_ms;
use anyhow::Result;
use tauri::{AppHandle, Emitter, Manager};
use url::Url;

/// The longest a whole sync may take (it holds the scan lock).
pub const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Stores whose account works through a cookie session.
pub const COOKIE_STORES: [AccountStore; 3] = [
    AccountStore::Gog,
    AccountStore::Battlenet,
    AccountStore::Humble,
];

pub fn is_cookie_store(store: AccountStore) -> bool {
    COOKIE_STORES.contains(&store)
}

/// The hosts requests may go to (exact names).
pub fn api_hosts(store: AccountStore) -> &'static [&'static str] {
    match store {
        AccountStore::Gog => gog::API_HOSTS,
        AccountStore::Battlenet => battlenet::API_HOSTS,
        AccountStore::Humble => humble::API_HOSTS,
        _ => &[],
    }
}

/// The URLs whose cookies are read from the sign-in window: every API host's root and the
/// store's own request URLs (a cookie set for a path is only sent there).
pub fn cookie_urls(store: AccountStore) -> Vec<String> {
    let requests: &[&str] = match store {
        AccountStore::Gog => &[
            "https://menu.gog.com/v1/account/basic",
            "https://embed.gog.com/user/data/games",
            "https://www.gog.com/u/",
        ],
        AccountStore::Battlenet => &[
            "https://account.battle.net/api/games-and-subs",
            "https://account.battle.net/api/classic-games",
        ],
        AccountStore::Humble => &[
            "https://www.humblebundle.com/home/library",
            "https://www.humblebundle.com/api/v1/orders",
        ],
        _ => &[],
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
        AccountStore::Gog => gog::COOKIE_DOMAINS,
        AccountStore::Battlenet => battlenet::COOKIE_DOMAINS,
        AccountStore::Humble => humble::COOKIE_DOMAINS,
        _ => &[],
    }
}

/// The first page of the store's sign-in window.
pub fn start_url(store: AccountStore) -> Option<Url> {
    let url = match store {
        AccountStore::Gog => gog::START_URL,
        AccountStore::Battlenet => battlenet::START_URL,
        AccountStore::Humble => humble::START_URL,
        _ => return None,
    };
    Url::parse(url).ok()
}

/// Whether the sign-in window shows the store's signed-in page (time to take the cookies).
pub fn signed_in_page(store: AccountStore, url: &Url) -> bool {
    match store {
        AccountStore::Gog => gog::signed_in_page(url),
        AccountStore::Battlenet => battlenet::signed_in_page(url),
        AccountStore::Humble => humble::signed_in_page(url),
        _ => false,
    }
}

/// Checks a session; the account name when the store gives one (GOG).
pub async fn probe(store: AccountStore, http: &StoreHttp) -> Result<Option<String>, FetchError> {
    match store {
        AccountStore::Gog => gog::probe(http).await.map(Some),
        AccountStore::Battlenet => battlenet::probe(http).await,
        AccountStore::Humble => humble::probe(http).await,
        _ => Err(FetchError::Invalid("this store has no account sign-in.")),
    }
}

async fn fetch(store: AccountStore, http: &StoreHttp) -> Result<FetchOutcome, FetchError> {
    match store {
        AccountStore::Gog => gog::fetch(http).await,
        AccountStore::Battlenet => battlenet::fetch(http).await,
        AccountStore::Humble => humble::fetch(http).await,
        _ => Err(FetchError::Invalid("this store has no account sign-in.")),
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
    let (jar, user_agent) = saved.jar(cookie_domains(store));
    let http = StoreHttp::new(api_hosts(store), jar, &user_agent)
        .map_err(|e| anyhow::anyhow!(e.message_for(store.name())))?;
    let outcome = tokio::time::timeout(SYNC_TIMEOUT, fetch(store, &http))
        .await
        .unwrap_or(Err(FetchError::Network(
            "the store took too long to answer.",
        )));
    let status = {
        let mut conn = state.db.lock();
        let status =
            apply::apply_account(&mut conn, store, &outcome, now_ms(), http.retry_after())?;
        // The store may have refreshed cookies: seal the jar again (only while signed in).
        if session::exists(&conn, store)? {
            let refreshed = Session::cookies(
                &http.cookies(),
                http.user_agent(),
                saved.account().map(str::to_string),
                now_ms(),
            );
            session::save(&conn, store, &refreshed)?;
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
    for store in COOKIE_STORES {
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

/// "Sync now".
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
        for store in COOKIE_STORES {
            let urls = cookie_urls(store);
            assert!(urls.len() > api_hosts(store).len());
            for url in urls {
                let url = Url::parse(&url).unwrap();
                assert_eq!(url.scheme(), "https");
                assert!(api_hosts(store).contains(&url.host_str().unwrap()), "{url}");
            }
        }
        assert!(cookie_urls(AccountStore::Steam).is_empty());
    }

    #[test]
    fn a_sync_is_bounded_in_time() {
        let source = include_str!("stores.rs");
        let sync = &source[source.find("pub async fn sync_store").unwrap()..];
        assert!(sync.contains("tokio::time::timeout(SYNC_TIMEOUT, fetch(store, &http))"));
        assert!(SYNC_TIMEOUT <= std::time::Duration::from_secs(300));
    }

    #[test]
    fn each_store_has_its_own_hosts_and_pages() {
        for store in COOKIE_STORES {
            assert!(!api_hosts(store).is_empty());
            let start = start_url(store).unwrap();
            // The first page is allowed in the sign-in window, and every API host's cookies are
            // kept by the cookie filter.
            assert!(super::super::signin::navigation_allowed(
                &start,
                super::super::signin::login_hosts(store)
            ));
            for host in api_hosts(store) {
                assert!(crate::accounts::cookies::domain_allowed(
                    host,
                    cookie_domains(store)
                ));
                assert!(
                    super::super::signin::host_allowed(
                        host,
                        super::super::signin::login_hosts(store)
                    ),
                    "{host}"
                );
            }
            // GOG and Battle.net start on the signed-in page itself (it sends a signed-out user
            // to the sign-in); Humble starts on its sign-in page.
            assert_eq!(signed_in_page(store, &start), store != AccountStore::Humble);
        }
        assert!(!is_cookie_store(AccountStore::Steam));
        assert!(start_url(AccountStore::Epic).is_none());
    }
}
