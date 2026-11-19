//! Store sign-in. One sign-in runs at a time ([`Flight`]); it ends with its result, with
//! `accounts_cancel_sign_in`, or after ten minutes, and the renderer only ever sees an
//! [`AccountStatus`](super::AccountStatus).
//!
//! **Steam signs in in the user's default web browser** (RFC 8252 style): Rust builds the
//! OpenID start URL and opens exactly that https URL with the system browser; Steam returns the
//! browser to a listener on `127.0.0.1:<random port>` ([`steam_openid::serve_return`]), which
//! checks the answer, shows a fixed page, stops listening and brings Game Lobby forward. The
//! embedded window below was dropped for Steam: Steam Guard's phone approval never completed
//! in it.
//!
//! **The sign-in window** (kept for later stores): Game Lobby's own window showing the store's
//! real sign-in page, so the password goes only to the store. It is locked down before the page
//! loads:
//!
//! - label `signin-<store>`, parented to `main`; no capability names it, so it can call no
//!   command, and the local protocols answer it with 403 (`lib.rs`);
//! - WebView2 InPrivate (`incognito`): no profile, cookies or cache on disk, a fresh session for
//!   every sign-in. The window refuses to open when InPrivate is not in effect;
//! - top-level navigation only to https pages of the store's sign-in hosts ([`navigation_allowed`]),
//!   checked twice (Tauri's handler and a native WebView2 one, which also cancels URIs `url`
//!   cannot parse), popups, downloads and permission requests denied, external app links
//!   (`steam://`, `mailto:`) cancelled, devtools, context menu, browser shortcuts and drag and
//!   drop off. No app script is added; Tauri's own core scripts (IPC bridge, `freezePrototype`)
//!   are injected into every webview, but every call from this one is refused;
//! - the title shows the host of the page being shown;
//! - one sign-in at a time, ended by its result, by closing the window (cancelled), by
//!   `accounts_cancel_sign_in` or after ten minutes. Rust always closes the window, and the
//!   renderer only ever sees an [`AccountStatus`](super::AccountStatus).

use super::steam_openid::{self, Expected, ReturnReply, SignInError};
use super::{lock, notify, AccountStore};
use crate::state::AppState;
use crate::types::now_ms;
use crate::windows::MAIN;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use url::Url;

/// A sign-in that is not finished after this long is cancelled and its window closed.
pub const FLIGHT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const LABEL_PREFIX: &str = "signin-";

/// The running sign-in.
pub struct Flight {
    pub id: u64,
    pub store: AccountStore,
    /// Steam OpenID: the nonce and `return_to` this sign-in expects back.
    pub expected: Option<Expected>,
    /// The answer was caught and is being checked; closing the window no longer cancels, and
    /// the timeout leaves it to finish.
    pub verifying: bool,
    /// The store page was requested: from then on `about:blank` is no longer allowed.
    pub loaded: bool,
    /// [`super::AccountsState::steam_generation`] when the sign-in started.
    pub generation: u64,
    /// The loopback listener (browser sign-in). Aborted when the flight ends early.
    listener: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Browser sign-in: the Rust-built start URL, opened again when the user asks a second time.
    browser_url: Option<Url>,
}

pub fn label(store: AccountStore) -> String {
    format!("{LABEL_PREFIX}{}", store.id())
}

/// Hosts (and their subdomains) a store's sign-in window may show. Third-party sign-in buttons
/// (Google, Apple, Facebook, ...) lead elsewhere and are blocked: users sign in with the store's
/// own e-mail and password.
pub fn login_hosts(store: AccountStore) -> &'static [&'static str] {
    match store {
        AccountStore::Steam => &["steamcommunity.com"],
        AccountStore::Gog => &["gog.com", "login.gog.com", "auth.gog.com"],
        AccountStore::Battlenet => &["battle.net", "blizzard.com"],
        AccountStore::Humble => &["humblebundle.com"],
        AccountStore::Xbox => &[
            "login.live.com",
            "login.microsoftonline.com",
            "account.live.com",
        ],
        AccountStore::Epic => &["epicgames.com"],
    }
}

/// How a store's sign-in happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    /// In the user's default web browser, back to a loopback listener (Steam).
    Browser,
    /// In Game Lobby's locked-down sign-in window (stores of later versions).
    Window,
}

fn method(store: AccountStore) -> Method {
    match store {
        AccountStore::Steam => Method::Browser,
        _ => Method::Window,
    }
}

/// The first page of a store's sign-in window. No store uses the window in this version.
fn window_start_url(_store: AccountStore) -> Option<Url> {
    None
}

/// The only URL the browser sign-in may hand to the system browser: Steam's OpenID endpoint
/// over https, built by Rust.
fn browser_url_ok(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("steamcommunity.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/openid/login"
}

/// Whether `host` (already lowercase ASCII, as `url` returns it) is one of `hosts` or a
/// subdomain of one. Punycode (`xn--`) labels are refused outright: look-alike names.
pub fn host_allowed(host: &str, hosts: &[&str]) -> bool {
    if host.is_empty()
        || !host.is_ascii()
        || host.ends_with('.')
        || host
            .split('.')
            .any(|l| l.is_empty() || l.starts_with("xn--"))
    {
        return false;
    }
    hosts.iter().any(|allowed| {
        host == *allowed
            || host
                .strip_suffix(allowed)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// Top-level navigation the sign-in window may make: https, no user name or password, no port,
/// a domain name (not an IP address) on the store's list.
pub fn navigation_allowed(url: &Url, hosts: &[&str]) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && matches!(url.host(), Some(url::Host::Domain(host)) if host_allowed(host, hosts))
}

/// The decision of the native WebView2 navigation handler, which sees every top-level navigation
/// even when Tauri's own handler lets one through unchecked (it allows URIs that `url` cannot
/// parse). Allowed: `about:blank` before the store page was requested, and pages
/// [`navigation_allowed`] accepts. Everything else is cancelled.
pub fn native_nav_ok(uri: &str, hosts: &[&str], blank_ok: bool) -> bool {
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    if uri == "about:blank" {
        return blank_ok;
    }
    navigation_allowed(&url, hosts)
}

/// Whether flight `id` may still show `about:blank` (only before its store page was requested).
fn blank_ok(app: &AppHandle, id: u64) -> bool {
    lock(&app.state::<AppState>().accounts.flight)
        .as_ref()
        .is_some_and(|f| f.id == id && !f.loaded)
}

/// Whether the ten-minute timeout ends flight `id` now: not while its answer is being checked
/// (that ends on its own within the request timeout).
fn times_out(flight: Option<&Flight>, id: u64) -> bool {
    flight.is_some_and(|f| f.id == id && !f.verifying)
}

/// Takes flight `id`'s answer once: only while nothing is being checked, and only when it
/// carries exactly this sign-in's nonce. Marks the flight as verifying and takes its listener
/// handle (the listener answers and stops on its own, so it must not be aborted). `None`: ignore
/// the request and keep waiting.
fn take_return(slot: &mut Option<Flight>, id: u64, url: &Url) -> Option<Expected> {
    let flight = slot.as_mut().filter(|f| f.id == id && !f.verifying)?;
    let expected = flight.expected.clone().filter(|e| e.carries_nonce(url))?;
    flight.verifying = true;
    flight.listener.take();
    Some(expected)
}

/// What a blocked navigation is logged as: scheme and host only (never a path or query).
fn describe(url: &Url) -> String {
    format!("{}://{}", url.scheme(), url.host_str().unwrap_or("-"))
}

fn title(store: AccountStore, host: &str) -> String {
    format!("Sign in to {} – {host}", store.name())
}

/// Starts the store's sign-in (or, when it is already running, brings it forward).
pub async fn start(app: &AppHandle, store: AccountStore) -> Result<(), String> {
    let state = app.state::<AppState>();
    // One flight: a second request for the same store keeps the running one; another store
    // waits. The lock is released before any window or browser call.
    let running = lock(&state.accounts.flight)
        .as_ref()
        .map(|f| (f.store, f.browser_url.clone()));
    if let Some((running, browser_url)) = running {
        if running == store {
            // A browser sign-in opens the same page again (same nonce, same listener).
            if let Some(url) = browser_url {
                if !open_in_browser(&url).await {
                    return Err(
                        SignInError::Window("Windows could not open your web browser.").message(),
                    );
                }
                return Ok(());
            }
            if let Some(window) = app.get_webview_window(&label(store)) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            return Ok(());
        }
        return Err(format!(
            "Finish or cancel the {} sign-in first.",
            running.name()
        ));
    }
    match method(store) {
        Method::Browser => start_browser(app, store).await,
        Method::Window => start_window(app, store).await,
    }
}

/// Reserves the one flight slot; the new flight's id.
fn reserve(
    state: &AppState,
    store: AccountStore,
    expected: Option<Expected>,
    loaded: bool,
) -> Result<u64, String> {
    let id = state.accounts.next_flight.fetch_add(1, Ordering::Relaxed) + 1;
    let generation = state.accounts.steam_generation.load(Ordering::SeqCst);
    let mut flight = lock(&state.accounts.flight);
    if flight.is_some() {
        return Err("Another sign-in is open.".into());
    }
    *flight = Some(Flight {
        id,
        store,
        expected,
        verifying: false,
        loaded,
        generation,
        listener: None,
        browser_url: None,
    });
    Ok(id)
}

/// Ends flight `id` after [`FLIGHT_TIMEOUT`], unless its answer is being checked.
fn spawn_timeout(app: &AppHandle, id: u64) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FLIGHT_TIMEOUT).await;
        let due = times_out(
            lock(&handle.state::<AppState>().accounts.flight).as_ref(),
            id,
        );
        if due {
            finish(&handle, id, Err(SignInError::TimedOut));
        }
    });
}

/// Opens a Rust-built sign-in URL in the default web browser. Only Steam's OpenID endpoint
/// ([`browser_url_ok`]) is ever opened; this is the one place that hands a URL to the browser.
async fn open_in_browser(url: &Url) -> bool {
    browser_url_ok(url) && crate::platform::shell_open(url.to_string()).await.is_ok()
}

/// Steam: listens on `127.0.0.1:<random port>`, then opens the Rust-built OpenID URL in the
/// default web browser. The URL is never logged (it carries the nonce).
async fn start_browser(app: &AppHandle, store: AccountStore) -> Result<(), String> {
    let state = app.state::<AppState>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "Could not prepare the sign-in (no local port).".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|_| "Could not prepare the sign-in (no local port).".to_string())?
        .port();
    let expected = Expected::new(
        &format!("http://127.0.0.1:{port}"),
        steam_openid::new_nonce(),
    );
    let start = expected.start_url();
    let id = reserve(&state, store, Some(expected), true)?;
    let handle = app.clone();
    let task = tauri::async_runtime::spawn(steam_openid::serve_return(listener, move |url| {
        let app = handle.clone();
        async move { browser_return(&app, id, url).await }
    }));
    match lock(&state.accounts.flight).as_mut() {
        Some(flight) if flight.id == id => {
            flight.listener = Some(task);
            flight.browser_url = Some(start.clone());
        }
        _ => task.abort(),
    }
    spawn_timeout(app, id);
    if !open_in_browser(&start).await {
        let e = SignInError::Window("Windows could not open your web browser.");
        finish(app, id, Err(e.clone()));
        return Err(e.message());
    }
    log::info!(
        "{} sign-in opened in the web browser; waiting on a local port.",
        store.name()
    );
    notify(app);
    Ok(())
}

/// The browser came back to the listener: takes the answer once, checks it with Steam, ends
/// the flight and brings Game Lobby forward. What the listener shows depends on the result.
async fn browser_return(app: &AppHandle, id: u64, url: Url) -> ReturnReply {
    let state = app.state::<AppState>();
    let taken = take_return(&mut lock(&state.accounts.flight), id, &url);
    let Some(expected) = taken else {
        log::info!("Steam sign-in: ignored a request that is not this sign-in's answer.");
        return ReturnReply::Ignored;
    };
    notify(app);
    let result = steam_openid::verify(
        &steam_openid::Endpoint::production(),
        &url,
        &expected,
        now_ms(),
        &state.accounts.seen_nonces,
    )
    .await;
    let reply = match result {
        Err(e) => {
            finish(app, id, Err(e));
            ReturnReply::Failed
        }
        Ok(steam_id) => match end_flight(app, id) {
            // Cancelled (or timed out) while Steam was being asked: nothing is saved.
            None => ReturnReply::Unsaved,
            Some(flight) => {
                page_for(save_sign_in(app, flight.store, flight.generation, &steam_id).await)
            }
        },
    };
    if let Err(e) = crate::windows::focus_main(app) {
        log::warn!("Could not bring Game Lobby forward: {e}");
    }
    reply
}

/// The page the browser gets for a saved, skipped or failed save.
fn page_for(saved: Saved) -> ReturnReply {
    match saved {
        Saved::Yes => ReturnReply::SignedIn,
        Saved::SignedOut => ReturnReply::Unsaved,
        Saved::Failed => ReturnReply::Failed,
    }
}

/// Opens the store's sign-in window. Unused in this version (no store signs in in a window).
async fn start_window(app: &AppHandle, store: AccountStore) -> Result<(), String> {
    let state = app.state::<AppState>();
    let Some(start) = window_start_url(store) else {
        return Err(store.unavailable());
    };
    let id = reserve(&state, store, None, false)?;
    let window = match open_window(app, store, id).await {
        Ok(window) => window,
        Err(e) => {
            finish(app, id, Err(e.clone()));
            return Err(e.message());
        }
    };
    spawn_timeout(app, id);
    if let Some(flight) = lock(&state.accounts.flight).as_mut().filter(|f| f.id == id) {
        flight.loaded = true;
    }
    if let Err(e) = window.navigate(start) {
        log::warn!("Could not load the sign-in page: {e}");
        finish(app, id, Err(SignInError::Window("the page did not load.")));
        return Err(SignInError::Window("the page did not load.").message());
    }
    log::info!("{} sign-in window opened.", store.name());
    notify(app);
    Ok(())
}

/// Builds the window on `about:blank`, then locks the webview down; the caller loads the store
/// page only after that succeeded.
async fn open_window(
    app: &AppHandle,
    store: AccountStore,
    id: u64,
) -> Result<WebviewWindow, SignInError> {
    let main = app
        .get_webview_window(MAIN)
        .ok_or(SignInError::Window("the main window is closed."))?;
    if let Some(old) = app.get_webview_window(&label(store)) {
        let _ = old.destroy();
    }
    let blank = Url::parse("about:blank").expect("valid URL");
    let hosts = login_hosts(store);
    let nav_app = app.clone();
    let window = WebviewWindowBuilder::new(app, label(store), WebviewUrl::External(blank))
        .title(title(store, hosts[0]))
        .inner_size(520.0, 720.0)
        .min_inner_size(400.0, 520.0)
        .center()
        .parent(&main)
        .map_err(|_| SignInError::Window("it could not be attached to Game Lobby."))?
        .incognito(true)
        .devtools(false)
        .disable_drag_drop_handler()
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_download(|_, _| false)
        .on_navigation(move |url| guard(&nav_app, id, store, url))
        .on_page_load(move |window, payload| {
            if let Some(host) = payload.url().host_str() {
                if payload.event() == PageLoadEvent::Started
                    || payload.event() == PageLoadEvent::Finished
                {
                    let _ = window.set_title(&title(store, host));
                }
            }
        })
        .build()
        .map_err(|e| {
            log::warn!("Could not create the sign-in window: {e}");
            SignInError::Window("Windows refused to create it.")
        })?;
    let closed = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            window_closed(&closed, id);
        }
    });
    if let Err(e) = harden(&window, app, id, store).await {
        let _ = window.destroy();
        return Err(e);
    }
    Ok(window)
}

/// The navigation guard (runs on the UI thread; never blocks).
fn guard(app: &AppHandle, id: u64, store: AccountStore, url: &Url) -> bool {
    if url.as_str() == "about:blank" {
        return blank_ok(app, id);
    }
    if navigation_allowed(url, login_hosts(store)) {
        return true;
    }
    log::info!(
        "{} sign-in window: blocked a page outside the sign-in hosts ({}).",
        store.name(),
        describe(url)
    );
    false
}

/// The window is gone (closed by the user, or by [`finish`]). Runs off the UI thread: `finish`
/// reads the library and must not run inside the window's own event.
fn window_closed(app: &AppHandle, id: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let verifying = lock(&app.state::<AppState>().accounts.flight)
            .as_ref()
            .is_some_and(|f| f.id == id && f.verifying);
        if !verifying {
            finish(&app, id, Err(SignInError::Cancelled));
        }
    });
}

/// Cancels the running sign-in, whatever its store.
pub fn cancel(app: &AppHandle) {
    let id = lock(&app.state::<AppState>().accounts.flight)
        .as_ref()
        .map(|f| f.id);
    if let Some(id) = id {
        finish(app, id, Err(SignInError::Cancelled));
    }
}

/// Cancels the running sign-in if it is for `store`.
pub fn cancel_store(app: &AppHandle, store: AccountStore) {
    let id = lock(&app.state::<AppState>().accounts.flight)
        .as_ref()
        .filter(|f| f.store == store)
        .map(|f| f.id);
    if let Some(id) = id {
        finish(app, id, Err(SignInError::Cancelled));
    }
}

/// Ends flight `id` (a later or earlier flight is left alone): stores the result, closes the
/// window and tells the main window.
fn finish(app: &AppHandle, id: u64, result: Result<String, SignInError>) {
    let state = app.state::<AppState>();
    let Some(flight) = end_flight(app, id) else {
        return;
    };
    let store = flight.store;
    match result {
        Ok(steam_id) => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                save_sign_in(&app, store, flight.generation, &steam_id).await;
            });
            return;
        }
        Err(e) => {
            let message = e.message();
            if e == SignInError::Cancelled {
                log::info!("{} sign-in cancelled.", store.name());
            } else {
                log::warn!("{} sign-in failed: {message}", store.name());
            }
            state.accounts.set_notice(store, false, message);
        }
    }
    notify(app);
}

/// Takes flight `id` out of the slot (a later or earlier flight is left alone), aborts its
/// listener if it still has one and closes its window. `None` when it already ended.
fn end_flight(app: &AppHandle, id: u64) -> Option<Flight> {
    let state = app.state::<AppState>();
    let flight = {
        let mut slot = lock(&state.accounts.flight);
        match slot.as_ref() {
            Some(f) if f.id == id => slot.take(),
            _ => None,
        }
    }?;
    if let Some(listener) = &flight.listener {
        listener.abort();
    }
    if let Some(window) = app.get_webview_window(&label(flight.store)) {
        let _ = window.destroy();
    }
    Some(flight)
}

/// What [`save_sign_in`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Saved {
    Yes,
    /// Signed out since the sign-in started: nothing saved.
    SignedOut,
    /// Windows secure storage or the library failed.
    Failed,
}

/// Whether a sign-in that started at `generation` may still save: no sign-out happened since.
fn still_wanted(generation: u64, current: u64) -> bool {
    generation == current
}

/// Saves a verified sign-in under the scan lock (which sign-out and imports also hold), unless
/// the account was signed out since the sign-in started; then starts a sync in the background
/// when the key is saved. Says whether it saved.
async fn save_sign_in(
    app: &AppHandle,
    store: AccountStore,
    generation: u64,
    steam_id: &str,
) -> Saved {
    let state = app.state::<AppState>();
    let saved = {
        let _guard = state.scan_lock.lock().await;
        if still_wanted(
            generation,
            state.accounts.steam_generation.load(Ordering::SeqCst),
        ) {
            Some(signed_in(&state, steam_id))
        } else {
            None
        }
    };
    let outcome = match &saved {
        None => Saved::SignedOut,
        Some(Ok(_)) => Saved::Yes,
        Some(Err(_)) => Saved::Failed,
    };
    match saved {
        None => {
            log::info!("{} sign-in discarded: signed out meanwhile.", store.name());
            state.accounts.set_notice(
                store,
                false,
                "You signed out while the sign-in was being checked; nothing was saved.".into(),
            );
        }
        Some(Ok(key_set)) => {
            log::info!("{} sign-in finished.", store.name());
            state.accounts.set_notice(
                store,
                true,
                if key_set {
                    "Signed in to Steam.".into()
                } else {
                    "Signed in to Steam. Add a free Steam Web API key to list the games you own."
                        .into()
                },
            );
            if key_set {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::steam_owned::sync_now(&app).await {
                        log::warn!("Steam owned games: {e:#}");
                    }
                    notify(&app);
                });
            }
        }
        Some(Err(e)) => {
            log::error!("Could not save the Steam sign-in: {e:#}");
            state.accounts.set_notice(
                store,
                false,
                "The sign-in worked but could not be saved (Windows secure storage).".into(),
            );
        }
    }
    notify(app);
    outcome
}

/// Stores the SteamID64 from a verified sign-in (the caller holds the scan lock). True when the
/// Web API key is saved too.
fn signed_in(state: &AppState, steam_id: &str) -> anyhow::Result<bool> {
    let old = crate::secrets::get_steam_owned_steam_id(state)?;
    let changed = prepare_for_account(&mut state.db.lock(), old.as_deref(), steam_id)?;
    if changed {
        log::info!("Steam sign-in: another account than before; its owned games were removed.");
    }
    crate::secrets::set_steam_owned_credentials(state, "", steam_id)?;
    crate::secrets::has_steam_owned_key(state)
}

/// Gets the library ready for the Steam account `new`: when another account was saved, the
/// not-installed games its import added are removed (games with your data stay, as with
/// Remove); either way the import is resumed and its throttle reset. True when the account
/// changed.
pub(crate) fn prepare_for_account(
    conn: &mut rusqlite::Connection,
    old: Option<&str>,
    new: &str,
) -> anyhow::Result<bool> {
    let changed = old.is_some_and(|old| old != new);
    if changed {
        crate::steam_owned::undo(conn)?;
    }
    super::resume_steam_import(conn)?;
    let mut import = crate::steam_owned::load_import_state(conn)?;
    import.attempted_at = 0;
    import.status = None;
    crate::steam_owned::save_import_state(conn, &import)?;
    Ok(changed)
}

/// Locks the WebView2 instance down before any store page loads: InPrivate must really be on,
/// and external app links are cancelled (not even the "open this app?" prompt is shown).
#[cfg(windows)]
async fn harden(
    window: &WebviewWindow,
    app: &AppHandle,
    id: u64,
    store: AccountStore,
) -> Result<(), SignInError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let app = app.clone();
    window
        .with_webview(move |webview| {
            let _ = tx.send(harden_webview2(&webview, app, id, store));
        })
        .map_err(|_| SignInError::Window("its web view is not ready."))?;
    rx.await
        .map_err(|_| SignInError::Window("its web view is not ready."))?
}

#[cfg(windows)]
fn harden_webview2(
    webview: &tauri::webview::PlatformWebview,
    app: AppHandle,
    id: u64,
    store: AccountStore,
) -> Result<(), SignInError> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Settings3, ICoreWebView2Settings4, ICoreWebView2_13, ICoreWebView2_18,
        COREWEBVIEW2_PERMISSION_STATE_DENY,
    };
    use webview2_com::{
        take_pwstr, LaunchingExternalUriSchemeEventHandler, NavigationStartingEventHandler,
        PermissionRequestedEventHandler,
    };
    use windows_core::Interface;
    const OLD_RUNTIME: SignInError = SignInError::Window(
        "the Microsoft Edge WebView2 Runtime is too old for a private sign-in window. Update it and try again.",
    );
    // SAFETY: plain COM calls on the live controller of this window, on the UI thread
    // (`with_webview` runs the closure there).
    unsafe {
        let core = webview
            .controller()
            .CoreWebView2()
            .map_err(|_| OLD_RUNTIME)?;
        let profile = core
            .cast::<ICoreWebView2_13>()
            .and_then(|v| v.Profile())
            .map_err(|_| OLD_RUNTIME)?;
        let mut private = windows_core::BOOL::default();
        profile
            .IsInPrivateModeEnabled(&mut private)
            .map_err(|_| OLD_RUNTIME)?;
        if !private.as_bool() {
            return Err(OLD_RUNTIME);
        }
        let core18 = core.cast::<ICoreWebView2_18>().map_err(|_| OLD_RUNTIME)?;
        let mut token = 0i64;
        core18
            .add_LaunchingExternalUriScheme(
                &LaunchingExternalUriSchemeEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|_| OLD_RUNTIME)?;
        // A second navigation check on WebView2 itself, added after Tauri's: Tauri lets a URI
        // through unchecked when `url` cannot parse it; this one cancels it (it only ever
        // cancels, so it never overrides Tauri's own cancel of Steam's return address).
        let hosts = login_hosts(store);
        let mut token = 0i64;
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = windows_core::PWSTR::null();
                    args.Uri(&mut uri)?;
                    let uri = take_pwstr(uri);
                    if !native_nav_ok(&uri, hosts, blank_ok(&app, id)) {
                        args.SetCancel(true)?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )
        .map_err(|_| OLD_RUNTIME)?;
        // No camera, microphone, location, notification or other permission prompts.
        let mut token = 0i64;
        core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                }
                Ok(())
            })),
            &mut token,
        )
        .map_err(|_| OLD_RUNTIME)?;
        let settings = core.Settings().map_err(|_| OLD_RUNTIME)?;
        // No browser context menu (Back, Reload, Inspect, Save as...). Browser shortcuts (find,
        // print, reload, zoom, devtools) are off; WebView2 keeps editing keys such as Ctrl+C,
        // Ctrl+V, Ctrl+A and Ctrl+Z working.
        settings
            .SetAreDefaultContextMenusEnabled(false)
            .map_err(|_| OLD_RUNTIME)?;
        settings
            .cast::<ICoreWebView2Settings3>()
            .and_then(|s| s.SetAreBrowserAcceleratorKeysEnabled(false))
            .map_err(|_| OLD_RUNTIME)?;
        // No "save password?" or autofill prompts in Game Lobby's window.
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings4>() {
            let _ = settings.SetIsPasswordAutosaveEnabled(false);
            let _ = settings.SetIsGeneralAutofillEnabled(false);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
async fn harden(
    _window: &WebviewWindow,
    _app: &AppHandle,
    _id: u64,
    _store: AccountStore,
) -> Result<(), SignInError> {
    Err(SignInError::Window("sign-in windows need Windows."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str, store: AccountStore) -> bool {
        navigation_allowed(&Url::parse(url).unwrap(), login_hosts(store))
    }

    #[test]
    fn the_steam_window_stays_on_steamcommunity_over_https() {
        let steam = AccountStore::Steam;
        assert!(ok(
            "https://steamcommunity.com/openid/login?openid.mode=checkid_setup",
            steam
        ));
        assert!(ok("https://steamcommunity.com:443/login/home/", steam));
        assert!(ok("https://www.steamcommunity.com/", steam));
        for bad in [
            "http://steamcommunity.com/openid/login",
            "https://steamcommunity.com:8443/",
            "https://user@steamcommunity.com/",
            "https://user:pw@steamcommunity.com/",
            "https://steamcommunity.com.evil.example/",
            "https://evilsteamcommunity.com/",
            "https://steamcommunity.co/",
            "https://store.steampowered.com/login/",
            "https://accounts.google.com/",
            "https://xn--80ak6aa92e.com/",
            "https://xn--80ak6aa92e.steamcommunity.com/",
            "https://stеamcommunity.com/",
            "https://203.0.113.5/",
            "https://[::1]/",
            "data:text/html,<script>alert(1)</script>",
            "javascript:alert(1)",
            "file:///C:/Windows/win.ini",
            "steam://run/570",
            "ms-settings:privacy",
            "about:srcdoc",
        ] {
            assert!(!ok(bad, steam), "{bad}");
        }
    }

    #[test]
    fn host_matching_is_exact_or_a_real_subdomain() {
        let gog = AccountStore::Gog;
        assert!(ok("https://gog.com/", gog));
        assert!(ok("https://login.gog.com/auth", gog));
        assert!(ok("https://www.gog.com/account", gog));
        for bad in [
            "https://gog.com.evil.com/",
            "https://evilgog.com/",
            "https://gog.co/",
            "https://login.gog.com.evil.com/",
            "https://gоg.com/",
            "https://xn--80ak6aa92e.gog.com/",
        ] {
            assert!(!ok(bad, gog), "{bad}");
        }
        assert!(!host_allowed("gog.com.", &["gog.com"]));
        assert!(!host_allowed("a..gog.com", &["gog.com"]));
        assert!(!host_allowed("", &["gog.com"]));
        assert!(!host_allowed("gög.com", &["gög.com"]));
        // Every store's list is plain lowercase ASCII without wildcards.
        for store in AccountStore::ALL {
            for host in login_hosts(store) {
                assert!(host_allowed(host, login_hosts(store)), "{host}");
                assert_eq!(host.to_ascii_lowercase(), *host);
                assert!(!host.contains('*') && !host.starts_with('.'));
            }
        }
    }

    #[test]
    fn the_steam_return_address_is_never_a_page_the_window_shows() {
        let e = Expected::new("http://localhost", "f".repeat(64));
        let back = Url::parse(&format!("{}&openid.mode=id_res", e.return_to)).unwrap();
        assert!(e.is_return(&back));
        assert!(!navigation_allowed(&back, login_hosts(AccountStore::Steam)));
        assert!(
            !e.is_return(&Url::parse("http://localhost:8080/gamelobby/steam-return?n=1").unwrap())
        );
        assert!(!e.is_return(&Url::parse("https://localhost/gamelobby/steam-return?n=1").unwrap()));
        assert!(
            !e.is_return(&Url::parse("http://user@localhost/gamelobby/steam-return?n=1").unwrap())
        );
    }

    #[test]
    fn labels_titles_and_logs_show_no_secrets() {
        assert_eq!(label(AccountStore::Steam), "signin-steam");
        assert!(!crate::windows::is_main_label(&label(AccountStore::Steam)));
        assert_eq!(
            title(AccountStore::Steam, "steamcommunity.com"),
            "Sign in to Steam – steamcommunity.com"
        );
        let url = Url::parse("https://evil.example/path?openid.sig=abc&code=x").unwrap();
        assert_eq!(describe(&url), "https://evil.example");
    }

    #[test]
    fn the_window_is_built_locked_down() {
        let source = include_str!("signin.rs");
        let build = &source[source
            .find("WebviewWindowBuilder::new(app, label(store)")
            .unwrap()..];
        let build = &build[..build.find(".build()").unwrap()];
        for needed in [
            "WebviewUrl::External(blank)",
            ".parent(&main)",
            ".incognito(true)",
            ".devtools(false)",
            ".disable_drag_drop_handler()",
            ".on_new_window(|_, _| NewWindowResponse::Deny)",
            ".on_download(|_, _| false)",
            ".on_navigation(",
        ] {
            assert!(build.contains(needed), "{needed}");
        }
        assert!(!build.contains("initialization_script"));
        assert!(!build.contains("data_directory"));
        // The store page loads only after the webview was hardened.
        let start = &source[source.find("async fn start_window").unwrap()..];
        assert!(
            start.find("open_window(app, store, id)").unwrap()
                < start.find("window.navigate(start)").unwrap()
        );
        let open = &source[source.find("async fn open_window").unwrap()..];
        assert!(
            open.find("harden(&window, app, id, store)").unwrap()
                < open.find("Ok(window)").unwrap()
        );
        // about:blank stops being allowed just before the store page is requested.
        assert!(
            start.find("flight.loaded = true").unwrap()
                < start.find("window.navigate(start)").unwrap()
        );
        // The native WebView2 checks are in place and only ever cancel.
        let harden = &source[source.find("fn harden_webview2").unwrap()..];
        let harden = &harden[..harden.find("#[cfg(not(windows))]").unwrap()];
        for needed in [
            "core.add_NavigationStarting(",
            "native_nav_ok(&uri, hosts, blank_ok(&app, id))",
            "core.add_PermissionRequested(",
            "COREWEBVIEW2_PERMISSION_STATE_DENY",
            "SetAreDefaultContextMenusEnabled(false)",
            "SetAreBrowserAcceleratorKeysEnabled(false)",
            "add_LaunchingExternalUriScheme(",
            "IsInPrivateModeEnabled(",
        ] {
            assert!(harden.contains(needed), "{needed}");
        }
        assert!(!harden.contains("SetCancel(false)"));
        // A verified sign-in is saved under the scan lock, after the generation check.
        let save = &source[source.find("async fn save_sign_in").unwrap()..];
        assert!(
            save.find("state.scan_lock.lock().await").unwrap()
                < save.find("signed_in(&state").unwrap()
        );
        let owned = include_str!("../steam_owned.rs");
        let remove = &owned[owned.find("pub async fn remove_all").unwrap()..];
        assert!(
            remove.find("scan_lock.lock().await").unwrap()
                < remove.find("steam_generation").unwrap()
        );
        assert!(
            remove.find("steam_generation").unwrap()
                < remove.find("clear_steam_owned_credentials").unwrap()
        );
    }

    #[test]
    fn steam_signs_in_in_the_browser_with_a_rust_built_url() {
        assert_eq!(method(AccountStore::Steam), Method::Browser);
        for store in AccountStore::ALL
            .iter()
            .filter(|s| **s != AccountStore::Steam)
        {
            assert_eq!(method(*store), Method::Window);
            assert!(window_start_url(*store).is_none());
        }
        let e = Expected::new("http://127.0.0.1:50123", "a".repeat(64));
        let url = e.start_url();
        assert!(browser_url_ok(&url));
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["openid.realm"], "http://127.0.0.1:50123/");
        assert_eq!(
            q["openid.return_to"],
            format!(
                "http://127.0.0.1:50123/gamelobby/steam-return?n={}",
                "a".repeat(64)
            )
        );
        for bad in [
            "http://steamcommunity.com/openid/login",
            "https://steamcommunity.com.evil.example/openid/login",
            "https://steamcommunity.com:444/openid/login",
            "https://steamcommunity.com/login/home",
            "file:///C:/Windows/notepad.exe",
        ] {
            assert!(!browser_url_ok(&Url::parse(bad).unwrap()), "{bad}");
        }
        // The browser gets only the URL built here, opened by Rust (never by the renderer):
        // bound to 127.0.0.1, built from Expected, checked, then shell_open.
        let source = include_str!("signin.rs");
        let browser = &source[source.find("async fn start_browser").unwrap()..];
        let browser = &browser[..browser.find("\n}\n").unwrap_or(browser.len())];
        let at = |needle: &str| browser.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("TcpListener::bind(\"127.0.0.1:0\")") < at("Expected::new("));
        assert!(at("let start = expected.start_url();") < at("open_in_browser(&start)"));
        assert!(at("flight.browser_url = Some(start.clone())") < at("open_in_browser(&start)"));
        // The one opener checks the URL first; nothing else hands a URL to the browser.
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        assert_eq!(code.matches("platform::shell_open(").count(), 1);
        assert!(code.contains(
            "browser_url_ok(url) && crate::platform::shell_open(url.to_string()).await.is_ok()"
        ));
        assert!(!browser.contains("{start}"));
        // Asking again during a browser sign-in reopens the stored URL (same nonce).
        let again = &source[source.find("pub async fn start(").unwrap()..];
        let again = &again[..again.find("match method(store)").unwrap()];
        assert!(again.contains("f.browser_url.clone()"));
        assert!(again.contains("open_in_browser(&url)"));
        // Cancel and timeout end the flight through `end_flight`, which aborts the listener.
        let end = &source[source.find("fn end_flight(").unwrap()..];
        assert!(end.find("listener.abort()").is_some());
        let finish = &source[source.find("fn finish(").unwrap()..];
        assert!(finish.find("end_flight(app, id)").is_some());
        let timeout = &source[source.find("fn spawn_timeout").unwrap()..];
        assert!(timeout
            .find("finish(&handle, id, Err(SignInError::TimedOut))")
            .is_some());
    }

    #[test]
    fn only_this_sign_ins_answer_is_taken_and_only_once() {
        // The browser is told what really happened to the save.
        assert_eq!(page_for(Saved::Yes), ReturnReply::SignedIn);
        assert_eq!(page_for(Saved::SignedOut), ReturnReply::Unsaved);
        assert_eq!(page_for(Saved::Failed), ReturnReply::Failed);
        let source = include_str!("signin.rs");
        let back = &source[source.find("async fn browser_return").unwrap()..];
        let back = &back[..back.find("fn page_for").unwrap()];
        assert!(back.contains("None => ReturnReply::Unsaved"));
        assert!(back.contains("page_for(save_sign_in("));
        let id = 5;
        let url = |n: &str| {
            Url::parse(&format!(
                "http://127.0.0.1:50000/gamelobby/steam-return?n={n}&openid.mode=id_res"
            ))
            .unwrap()
        };
        let nonce = "a".repeat(64);
        let mut slot = Some(flight(id, false));
        // Wrong, missing or doubled nonce, or another flight: ignored, flight unchanged.
        for bad in [
            url(&"b".repeat(64)),
            Url::parse("http://127.0.0.1:50000/gamelobby/steam-return?openid.mode=id_res").unwrap(),
            Url::parse(&format!(
                "http://127.0.0.1:50000/gamelobby/steam-return?n={nonce}&n={nonce}"
            ))
            .unwrap(),
        ] {
            assert!(take_return(&mut slot, id, &bad).is_none());
            assert!(!slot.as_ref().unwrap().verifying);
        }
        assert!(take_return(&mut slot, id + 1, &url(&nonce)).is_none());
        // The right answer is taken once.
        assert!(take_return(&mut slot, id, &url(&nonce)).is_some());
        assert!(slot.as_ref().unwrap().verifying);
        assert!(take_return(&mut slot, id, &url(&nonce)).is_none());
        assert!(take_return(&mut None, id, &url(&nonce)).is_none());
        // A window flight (no OpenID) never takes one.
        let mut window = Some(Flight {
            expected: None,
            ..flight(id, false)
        });
        assert!(take_return(&mut window, id, &url(&nonce)).is_none());
    }

    #[test]
    fn the_native_navigation_check_refuses_what_url_cannot_parse() {
        let hosts = login_hosts(AccountStore::Steam);
        for bad in [
            "https://exa mple.com",
            "https://a|b.com/",
            "https://xn--a.steamcommunity.com/",
            "http://localhost/gamelobby/steam-return?n=1",
            "steam://run/570",
            "",
            "about:blank#x",
        ] {
            assert!(!native_nav_ok(bad, hosts, true), "{bad}");
        }
        assert!(native_nav_ok(
            "https://steamcommunity.com/openid/login?x=1",
            hosts,
            false
        ));
        // about:blank only before the store page was requested.
        assert!(native_nav_ok("about:blank", hosts, true));
        assert!(!native_nav_ok("about:blank", hosts, false));
    }

    fn flight(id: u64, verifying: bool) -> Flight {
        Flight {
            id,
            store: AccountStore::Steam,
            expected: Some(Expected::new("http://127.0.0.1:50000", "a".repeat(64))),
            verifying,
            loaded: true,
            generation: 0,
            listener: None,
            browser_url: None,
        }
    }

    #[test]
    fn the_timeout_leaves_a_sign_in_being_checked_alone() {
        assert!(times_out(Some(&flight(3, false)), 3));
        assert!(!times_out(Some(&flight(3, true)), 3));
        assert!(!times_out(Some(&flight(4, false)), 3));
        assert!(!times_out(None, 3));
        // A sign-out after the sign-in started wins.
        assert!(still_wanted(7, 7));
        assert!(!still_wanted(7, 8));
    }

    #[test]
    fn signing_in_to_another_steam_account_removes_the_old_accounts_games() {
        use crate::steam_owned::{apply, load_import_state, OwnedApp, OwnedList};
        let list = |ids: &[&str]| OwnedList {
            ids: ids.iter().map(|i| i.to_string()).collect(),
            apps: ids
                .iter()
                .map(|i| OwnedApp {
                    app_id: i.to_string(),
                    name: format!("Game {i}"),
                })
                .collect(),
            playtime: Default::default(),
            last_played: Default::default(),
        };
        let database = crate::db::Db::memory().unwrap();
        let mut conn = database.lock();
        let old = "76561197960287930";
        let new = "76561197960287931";
        apply(&mut conn, &Ok(list(&["10", "20"])), 1000).unwrap();
        conn.execute("UPDATE games SET favorite=1 WHERE id='steam:20'", [])
            .unwrap();
        // The same account again: nothing removed, the throttle reset.
        assert!(!prepare_for_account(&mut conn, Some(old), old).unwrap());
        assert_eq!(crate::db::game_count(&conn).unwrap(), 2);
        let state = load_import_state(&conn).unwrap();
        assert_eq!((state.attempted_at, state.status), (0, None));
        // No account before: nothing removed.
        assert!(!prepare_for_account(&mut conn, None, new).unwrap());
        assert_eq!(crate::db::game_count(&conn).unwrap(), 2);
        // Another account: the old import's games go, the favourite stays, the pause is lifted.
        super::super::save_mode(
            &conn,
            AccountStore::Steam,
            super::super::AccountMode::Installed,
        )
        .unwrap();
        assert!(prepare_for_account(&mut conn, Some(old), new).unwrap());
        assert!(crate::db::get_game(&conn, "steam:10").unwrap().is_none());
        assert!(crate::db::get_game(&conn, "steam:20").unwrap().is_some());
        assert!(load_import_state(&conn).unwrap().app_ids.is_empty());
        assert!(!super::super::steam_import_paused(&conn).unwrap());
    }
}
